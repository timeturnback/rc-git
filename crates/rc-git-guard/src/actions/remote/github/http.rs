//! The one HTTP client in rc-git that talks to an API, on libcurl. Every request goes through
//! [`send`], which fixes the transport rules in one place:
//!
//! - the URL is an [`ApiUrl`] (built from the account's host only) and must be `https://`, or
//!   `http://` to a loopback host under the dev allowance; curl's protocol list cannot be set
//!   through the safe `curl` API, so this check is the restriction, and with redirects off the
//!   request URL is the only one curl ever connects to;
//! - redirects are never followed, and a 3xx answer is an error. One call site, the job log's
//!   ([`send_reading_redirect`], called only by `Session::job_log_location`), receives a 3xx's
//!   `Location` instead, and follows it by hand at most once, through [`download`];
//! - [`download`] is the one request to a host other than the account's: a
//!   [`DownloadRequest`] to a [`LogDownloadUrl`], which has no token field, so it carries no
//!   `Authorization` header and no GitHub API header, follows no redirect either, and keeps at
//!   most its cap of the body instead of failing over it;
//! - peer and host verification stay at curl's defaults (on); `clippy.toml` bans the setters
//!   that turn them off, so there is no switch;
//! - `.netrc` is ignored, and nothing is read from the repository: no proxy, header, CA or
//!   credential helper can come from its config. libcurl still honors the process
//!   environment's proxy variables, as fetch and push do; a loopback host bypasses them;
//! - connect, idle and total time are bounded, the body and the headers are capped, and the
//!   caller's [`Cancel`] stops the call: the wait returns within [`CANCEL_POLL`], and curl's
//!   progress callback aborts the abandoned transfer at its next tick.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use curl::easy::{Easy2, Handler, List, NetRc, WriteError};

use super::download::LogDownloadUrl;
use super::host::{ApiUrl, PLAIN_HTTP, PlainHttp};
use super::secret::ApiToken;
use super::text::server_text;
use super::{GitHubError, GitHubResult};
use crate::actions::remote::{CANCEL_POLL, Cancel};

/// The `User-Agent` every request carries; GitHub refuses requests without one.
const USER_AGENT: &str = "rc-git";

/// The REST API version every API request asks for. 2022-11-28 is supported on github.com
/// until 2028 and on every supported GitHub Enterprise Server release.
const API_VERSION: &str = "2022-11-28";

/// The most header bytes one response may carry, every intermediate response included.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// The most headers one response may carry.
const MAX_HEADERS: usize = 200;

/// The request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    Get,
    Post,
}

/// A request body and its media type. Its `Debug` shows the kind and the length only: a form
/// carries a device code or a refresh token.
#[derive(Clone)]
pub(super) enum Body {
    /// `application/x-www-form-urlencoded`, for the OAuth endpoints.
    Form(Vec<u8>),
    /// `application/json`, for the REST and GraphQL APIs.
    Json(Vec<u8>),
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, bytes) = match self {
            Self::Form(bytes) => ("Form", bytes),
            Self::Json(bytes) => ("Json", bytes),
        };
        write!(f, "{kind}(<{} bytes redacted>)", bytes.len())
    }
}

/// One request, owned, so it can move onto the transfer thread. Its `Debug` shows neither the
/// token, the body nor the secrets to scrub.
#[derive(Clone)]
pub(super) struct Request {
    /// What is being done, for messages.
    pub op: &'static str,
    pub method: Method,
    pub url: ApiUrl,
    /// The `Accept` header.
    pub accept: &'static str,
    /// Sent as `Authorization: Bearer`, when set.
    pub token: Option<ApiToken>,
    /// Whether to send `X-GitHub-Api-Version` (the API, not the OAuth endpoints).
    pub api_version: bool,
    pub body: Option<Body>,
    /// The most body bytes accepted; more is [`GitHubError::TooLarge`].
    pub limit: usize,
    /// Secrets the request carries besides `token` (a device code, a refresh token), scrubbed
    /// from every message the call produces.
    pub scrub: Vec<String>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("op", &self.op)
            .field("method", &self.method)
            .field("url", &self.url)
            .field("accept", &self.accept)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("api_version", &self.api_version)
            .field("body", &self.body)
            .field("limit", &self.limit)
            .field("scrub", &format_args!("<{} redacted>", self.scrub.len()))
            .finish()
    }
}

/// A response with a status outside 3xx. Its `Debug` shows the status, the header names and
/// the body's length: a token endpoint's body is a token, and a sign-in page sets cookies.
#[derive(Clone)]
pub(super) struct Response {
    pub status: u32,
    /// Header names lowercase, values trimmed, in arrival order, of the final response only.
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &names)
            .field(
                "body",
                &format_args!("<{} bytes redacted>", self.body.len()),
            )
            .finish()
    }
}

impl Response {
    /// The first value of header `name` (lowercase).
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The job log's download: one `GET` to the address GitHub's redirect named, with no token. The
/// type has no token field and its headers are built from `limit` and `tail` alone
/// ([`download_headers`]), so no `Authorization` header and no GitHub API header can reach the
/// storage host. Its `Debug` names the host only.
#[derive(Debug, Clone)]
pub(super) struct DownloadRequest {
    /// What is being done, for messages.
    pub op: &'static str,
    pub url: LogDownloadUrl,
    /// The most body bytes kept: a host that sends more has the body cut there.
    pub limit: usize,
    /// Whether to ask for the last `limit` bytes (`Range: bytes=-<limit>`); otherwise the request
    /// asks for the body from its start, and it is cut at `limit`.
    pub tail: bool,
}

/// A redirect's `Location`, as the host sent it: a secret (a job log's signed address) until
/// [`LogDownloadUrl::parse`] judges it. Its `Debug` shows its length only.
#[derive(Clone)]
pub(super) struct Location(String);

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Location(<{} bytes redacted>)", self.0.len())
    }
}

impl Location {
    /// The text, for [`LogDownloadUrl::parse`].
    pub(super) fn text(&self) -> &str {
        &self.0
    }
}

/// What [`send_reading_redirect`] got back: a response outside 3xx, or a redirect and its
/// `Location`, which nothing follows.
#[derive(Debug)]
pub(super) enum Answered {
    /// A status outside 3xx.
    Response(Response),
    /// A 3xx, with its `Location` when it sent one.
    Redirect {
        /// The status.
        status: u32,
        /// The `Location` header.
        location: Option<Location>,
    },
}

/// What a download got back. `cut` says the body stopped at the request's limit and the rest was
/// never received.
#[derive(Clone)]
pub(super) struct Downloaded {
    pub status: u32,
    /// `Content-Range`, when the host sent one.
    pub content_range: Option<String>,
    pub body: Vec<u8>,
    pub cut: bool,
}

impl fmt::Debug for Downloaded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Downloaded")
            .field("status", &self.status)
            .field("content_range", &self.content_range)
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .field("cut", &self.cut)
            .finish()
    }
}

/// Whether `url` may be sent to under `plain`: `https://`, or `http://` to a loopback host when
/// the dev allowance is on.
pub(super) fn allowed_url(url: &str, plain: PlainHttp) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let loopback = [
        "http://127.0.0.1:",
        "http://127.0.0.1/",
        "http://localhost:",
        "http://localhost/",
    ];
    plain == PlainHttp::Loopback && loopback.iter().any(|prefix| url.starts_with(prefix))
}

/// Sends `request` and waits for the answer on a transfer thread, looking at `cancel` every
/// [`CANCEL_POLL`]. A 3xx answer is never followed.
///
/// # Errors
/// [`GitHubError::Cancelled`], [`GitHubError::Transport`], [`GitHubError::TooLarge`],
/// [`GitHubError::Malformed`] for headers over their cap, [`GitHubError::Redirected`] for a
/// 3xx; any other status is the caller's to judge.
pub(super) fn send(request: Request, cancel: &Cancel) -> GitHubResult<Response> {
    match deliver(request, cancel) {
        Delivery::Sent(result) => result,
        Delivery::NotSent(error) => Err(error),
    }
}

/// [`send`] for the one call whose redirect is read instead of refused: GitHub answers a job log's
/// download with a `302` naming where the log is. Nothing here follows it; the caller judges the
/// `Location` ([`LogDownloadUrl::parse`]) and downloads from it once, without the token
/// ([`download`]). Only `Session::job_log_location` calls this (`scripts/check.sh` fails another
/// caller), and this is the only place a response's `Location` is read.
///
/// # Errors
/// As [`send`], except that a 3xx is [`Answered::Redirect`].
pub(super) fn send_reading_redirect(request: Request, cancel: &Cancel) -> GitHubResult<Answered> {
    let transfer = Transfer::Api {
        request,
        redirect: OnRedirect::Read,
    };
    let received = match run(transfer, cancel) {
        Delivery::Sent(result) => result?,
        Delivery::NotSent(error) => return Err(error),
    };
    if (300..400).contains(&received.status) {
        let location = received
            .header("location")
            .map(|value| Location(value.to_owned()));
        return Ok(Answered::Redirect {
            status: received.status,
            location,
        });
    }
    Ok(Answered::Response(received.into_response()))
}

/// Downloads what `request` names: one `GET`, asking for the last `limit` bytes
/// (`Range: bytes=-<limit>`) when `request.tail` says so, with no token and no GitHub API header,
/// following no redirect. A body longer than `limit` stops the transfer there and comes back
/// [`Downloaded::cut`], not as an error. Every message names the address as
/// [`LogDownloadUrl::shown`] does, never whole. Only `logs.rs`'s `download_log` calls it.
///
/// # Errors
/// [`GitHubError::Cancelled`], [`GitHubError::Transport`], [`GitHubError::Malformed`] for headers
/// over their cap, [`GitHubError::Redirected`] for a 3xx (nothing follows it); any other status is
/// the caller's to judge.
pub(super) fn download(request: DownloadRequest, cancel: &Cancel) -> GitHubResult<Downloaded> {
    let received = match run(Transfer::Download(request), cancel) {
        Delivery::Sent(result) => result?,
        Delivery::NotSent(error) => return Err(error),
    };
    Ok(Downloaded {
        status: received.status,
        content_range: received.header("content-range").map(str::to_owned),
        body: received.body,
        cut: received.cut,
    })
}

/// Whether a request may have reached the host, for a mutation, whose failure means one thing
/// when nothing left the machine and another when the host may have acted on it.
#[derive(Debug)]
pub(super) enum Delivery<T = Response> {
    /// Nothing left the machine: a cancel before the transfer started, a URL the transport
    /// refuses, or a failure to resolve, connect or finish the TLS handshake, all of which come
    /// before curl writes a byte of the request.
    NotSent(GitHubError),
    /// The request may have reached the host, and this came back: the response, or why none
    /// arrived (a timeout, a dropped connection, a cancel while it ran, a redirect, a body over
    /// its cap).
    Sent(GitHubResult<T>),
}

/// [`send`], telling apart a failure before the request left from one after. Every transport
/// rule is the same, since `send` is this function. A failure it cannot place (the request
/// thread did not start, or ended without a word) counts as sent: a mutation then reads what
/// happened instead of assuming nothing did.
pub(super) fn deliver(request: Request, cancel: &Cancel) -> Delivery {
    let transfer = Transfer::Api {
        request,
        redirect: OnRedirect::Refuse,
    };
    match run(transfer, cancel) {
        Delivery::NotSent(error) => Delivery::NotSent(error),
        Delivery::Sent(result) => Delivery::Sent(result.map(Received::into_response)),
    }
}

/// What a transfer does with a 3xx answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnRedirect {
    /// It is [`GitHubError::Redirected`].
    Refuse,
    /// It is handed back with its headers, for [`send_reading_redirect`].
    Read,
}

/// One transfer: an API request, or the job log's download.
#[derive(Debug)]
enum Transfer {
    Api {
        request: Request,
        redirect: OnRedirect,
    },
    Download(DownloadRequest),
}

impl Transfer {
    fn op(&self) -> &'static str {
        match self {
            Self::Api { request, .. } => request.op,
            Self::Download(request) => request.op,
        }
    }

    /// The URL curl connects to.
    fn url(&self) -> &str {
        match self {
            Self::Api { request, .. } => request.url.as_str(),
            Self::Download(request) => request.url.as_str(),
        }
    }

    /// The URL as messages name it: an API URL whole (it holds no secret), a download address as
    /// its host alone.
    fn shown(&self) -> String {
        match self {
            Self::Api { request, .. } => request.url.as_str().to_owned(),
            Self::Download(request) => request.url.shown(),
        }
    }

    fn limit(&self) -> usize {
        match self {
            Self::Api { request, .. } => request.limit,
            Self::Download(request) => request.limit,
        }
    }

    /// Whether a body over the limit is kept up to it ([`Downloaded::cut`]) rather than refused.
    fn keeps_partial(&self) -> bool {
        match self {
            Self::Api { .. } => false,
            Self::Download(_) => true,
        }
    }

    fn reads_redirect(&self) -> bool {
        match self {
            Self::Api { redirect, .. } => *redirect == OnRedirect::Read,
            Self::Download(_) => false,
        }
    }

    /// The secrets the transfer carries, for `server_text` to scrub.
    fn secrets(&self) -> Vec<&str> {
        match self {
            Self::Api { request, .. } => secrets(request),
            Self::Download(request) => request.url.secrets(),
        }
    }
}

/// What came back from a transfer: the status, the final response's headers, the body (cut at the
/// limit when the transfer keeps a partial body), and whether it was cut.
struct Received {
    status: u32,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    cut: bool,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    fn into_response(self) -> Response {
        Response {
            status: self.status,
            headers: self.headers,
            body: self.body,
        }
    }
}

/// Runs `transfer` on a transfer thread under every transport rule.
fn run(transfer: Transfer, cancel: &Cancel) -> Delivery<Received> {
    let op = transfer.op();
    if cancel.is_cancelled() {
        return Delivery::NotSent(GitHubError::Cancelled { op });
    }
    if !allowed_url(transfer.url(), PLAIN_HTTP) {
        return Delivery::NotSent(GitHubError::Transport {
            op,
            url: transfer.shown(),
            reason: "rc-git speaks only https:// to GitHub".to_owned(),
        });
    }
    let worker_cancel = cancel.clone();
    match detached(cancel, op, move || Ok(perform(&transfer, &worker_cancel))) {
        Ok(delivery) => delivery,
        Err(error) => Delivery::Sent(Err(error)),
    }
}

/// Runs `work` on a detached thread and waits for it, looking at `cancel` every
/// [`CANCEL_POLL`], as `run_detached` does for fetch and push: curl blocks without looking at
/// the flag, so the wait does. The abandoned thread ends at curl's next progress tick, which
/// sees the same flag.
fn detached<T, F>(cancel: &Cancel, op: &'static str, work: F) -> GitHubResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> GitHubResult<T> + Send + 'static,
{
    use std::sync::mpsc::{RecvTimeoutError, channel};
    let (done, finished) = channel();
    std::thread::Builder::new()
        .name(format!("rc-git {op}"))
        .spawn(move || {
            // The receiver is gone only when the caller already returned Cancelled.
            drop(done.send(work()));
        })
        .map_err(|source| GitHubError::Transport {
            op,
            url: String::new(),
            reason: format!("could not start the request thread: {source}"),
        })?;
    loop {
        match finished.recv_timeout(CANCEL_POLL) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => {
                if cancel.is_cancelled() {
                    return Err(GitHubError::Cancelled { op });
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(GitHubError::Transport {
                    op,
                    url: String::new(),
                    reason: "the request thread ended without an answer".to_owned(),
                });
            }
        }
    }
}

/// What curl hands back while a transfer runs: the capped body and headers, and the cancel
/// flag its progress callback polls.
struct Collector {
    body: Vec<u8>,
    limit: usize,
    /// Keep the body up to `limit` when more arrives (a download), instead of none of the write
    /// that crosses it (an API answer, which is then an error).
    keep_partial: bool,
    over_limit: bool,
    headers: Vec<(String, String)>,
    header_bytes: usize,
    headers_over_limit: bool,
    cancel: Arc<AtomicBool>,
}

impl Collector {
    fn new(limit: usize, keep_partial: bool, cancel: Arc<AtomicBool>) -> Self {
        Self {
            body: Vec::new(),
            limit,
            keep_partial,
            over_limit: false,
            headers: Vec::new(),
            header_bytes: 0,
            headers_over_limit: false,
            cancel,
        }
    }
}

impl Handler for Collector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        if self.body.len().saturating_add(data.len()) > self.limit {
            self.over_limit = true;
            if !self.keep_partial {
                // Taking fewer bytes than offered makes curl abort the transfer.
                return Ok(0);
            }
            let room = self.limit.saturating_sub(self.body.len()).min(data.len());
            self.body.extend_from_slice(&data[..room]);
            // Fewer than offered: curl stops the transfer, and nothing past the limit arrives.
            return Ok(room);
        }
        self.body.extend_from_slice(data);
        Ok(data.len())
    }

    fn header(&mut self, data: &[u8]) -> bool {
        self.header_bytes = self.header_bytes.saturating_add(data.len());
        if self.header_bytes > MAX_HEADER_BYTES || self.headers.len() >= MAX_HEADERS {
            self.headers_over_limit = true;
            return false;
        }
        if data.starts_with(b"HTTP/") {
            // A new response (after a 100 Continue or a proxy's CONNECT): its headers alone.
            self.headers.clear();
            return true;
        }
        let line = String::from_utf8_lossy(data);
        if let Some((name, value)) = line.split_once(':') {
            self.headers
                .push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
        true
    }

    fn progress(&mut self, _dltotal: f64, _dlnow: f64, _ultotal: f64, _ulnow: f64) -> bool {
        !self.cancel.load(Ordering::SeqCst)
    }
}

/// The secrets `request` carries, for `server_text` to scrub.
fn secrets(request: &Request) -> Vec<&str> {
    let mut secrets: Vec<&str> = request.scrub.iter().map(String::as_str).collect();
    if let Some(token) = &request.token {
        secrets.push(token.expose_secret());
    }
    secrets
}

/// The header list for an API `request`: the only place an `Authorization` header is written.
fn api_headers(request: &Request) -> Result<List, curl::Error> {
    let mut list = List::new();
    list.append(&format!("Accept: {}", request.accept))?;
    if request.api_version {
        list.append(&format!("X-GitHub-Api-Version: {API_VERSION}"))?;
    }
    if let Some(token) = &request.token {
        list.append(&format!("Authorization: Bearer {}", token.expose_secret()))?;
    }
    match &request.body {
        Some(Body::Form(_)) => list.append("Content-Type: application/x-www-form-urlencoded")?,
        Some(Body::Json(_)) => list.append("Content-Type: application/json")?,
        None => {}
    }
    // curl would otherwise add `Expect: 100-continue` to a large POST and wait for it.
    list.append("Expect:")?;
    Ok(list)
}

/// The header list for a download: the byte range asked for, when it asks for the tail, and
/// nothing else. It reads `request.limit` and `request.tail` alone; a [`DownloadRequest`] holds no
/// token to read.
fn download_headers(request: &DownloadRequest) -> Result<List, curl::Error> {
    let mut list = List::new();
    if request.tail {
        list.append(&format!("Range: bytes=-{}", request.limit))?;
    }
    Ok(list)
}

/// Builds the handle for `transfer`. Every option that bounds or restricts the transfer is set
/// here, so no request can leave without them.
fn configure(transfer: &Transfer, cancel: &Cancel) -> Result<Easy2<Collector>, curl::Error> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the GitHub client is the one sanctioned curl user; see docs/safety.md"
    )]
    let mut easy = Easy2::new(Collector::new(
        transfer.limit(),
        transfer.keeps_partial(),
        cancel.share(),
    ));
    easy.url(transfer.url())?;
    easy.follow_location(false)?;
    easy.netrc(NetRc::Ignored)?;
    easy.useragent(USER_AGENT)?;
    easy.connect_timeout(cancel.connect())?;
    easy.low_speed_limit(1)?;
    easy.low_speed_time(cancel.idle().max(std::time::Duration::from_secs(1)))?;
    easy.timeout(cancel.connect() + cancel.idle())?;
    if !transfer.keeps_partial() {
        easy.max_filesize(u64::try_from(transfer.limit()).unwrap_or(u64::MAX))?;
    }
    easy.progress(true)?;
    if transfer.url().starts_with("http://") {
        // A loopback test server: no environment proxy may stand in between.
        easy.proxy("")?;
    }
    match transfer {
        Transfer::Api { request, .. } => {
            easy.http_headers(api_headers(request)?)?;
            match (&request.method, &request.body) {
                (Method::Post, Some(Body::Form(bytes) | Body::Json(bytes))) => {
                    easy.post(true)?;
                    easy.post_fields_copy(bytes)?;
                }
                (Method::Post, None) => {
                    easy.post(true)?;
                    easy.post_fields_copy(&[])?;
                }
                (Method::Get, _) => easy.get(true)?,
            }
        }
        Transfer::Download(request) => {
            easy.http_headers(download_headers(request)?)?;
            easy.get(true)?;
        }
    }
    Ok(easy)
}

/// Whether curl failed before it wrote a byte of the request: it could not resolve the host or
/// the proxy, connect, or complete the TLS handshake, which comes before the request.
fn before_request(error: &curl::Error) -> bool {
    error.is_couldnt_resolve_host()
        || error.is_couldnt_resolve_proxy()
        || error.is_couldnt_connect()
        || error.is_ssl_connect_error()
        || error.is_peer_failed_verification()
}

/// One transfer, on the transfer thread.
fn perform(transfer: &Transfer, cancel: &Cancel) -> Delivery<Received> {
    let op = transfer.op();
    let url = transfer.shown();
    let secrets = transfer.secrets();
    let transport = |error: &curl::Error| GitHubError::Transport {
        op,
        url: url.clone(),
        reason: server_text(&describe(error), &secrets),
    };
    let easy = match configure(transfer, cancel) {
        Ok(easy) => easy,
        Err(error) => return Delivery::NotSent(transport(&error)),
    };
    let result = easy.perform();
    let collected = easy.get_ref();
    let cut = collected.over_limit && transfer.keeps_partial();
    if (collected.over_limit && !cut)
        || result
            .as_ref()
            .is_err_and(curl::Error::is_filesize_exceeded)
    {
        return Delivery::Sent(Err(GitHubError::TooLarge {
            op,
            url,
            limit: transfer.limit(),
        }));
    }
    if let Err(error) = &result
        && !cut
    {
        let aborted = error.is_aborted_by_callback() && !collected.headers_over_limit;
        if cancel.is_cancelled() || aborted {
            return Delivery::Sent(Err(GitHubError::Cancelled { op }));
        }
        if collected.headers_over_limit {
            return Delivery::Sent(Err(GitHubError::Malformed {
                op,
                url,
                reason: format!(
                    "the response headers are over {MAX_HEADER_BYTES} bytes or {MAX_HEADERS} lines"
                ),
            }));
        }
        if before_request(error) {
            return Delivery::NotSent(transport(error));
        }
        return Delivery::Sent(Err(transport(error)));
    }
    let status = match easy.response_code() {
        Ok(status) => status,
        Err(error) => return Delivery::Sent(Err(transport(&error))),
    };
    if (300..400).contains(&status) && !transfer.reads_redirect() {
        return Delivery::Sent(Err(GitHubError::Redirected { op, url, status }));
    }
    let collected = easy.get_ref();
    Delivery::Sent(Ok(Received {
        status,
        headers: collected.headers.clone(),
        body: collected.body.clone(),
        cut,
    }))
}

/// curl's description of `error` with its detail, e.g. `Couldn't connect to server: Failed to
/// connect to …`.
fn describe(error: &curl::Error) -> String {
    match error.extra_description() {
        Some(extra) => format!("{}: {extra}", error.description()),
        None => error.description().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_leaves_a_shipped_build() {
        for url in [
            "http://127.0.0.1:4010/x",
            "http://localhost/x",
            "http://example.com/x",
        ] {
            assert!(!allowed_url(url, PlainHttp::Refused), "{url}");
        }
        assert!(allowed_url("https://github.com/x", PlainHttp::Refused));
        assert!(!allowed_url("ftp://github.com/x", PlainHttp::Refused));
        assert!(!allowed_url("file:///etc/passwd", PlainHttp::Loopback));
    }

    fn collector(limit: usize) -> Collector {
        Collector::new(limit, false, Arc::new(AtomicBool::new(false)))
    }

    /// curl 8.4 and later stop at `max_filesize` even without a `Content-Length`; an older
    /// system libcurl does not, and the collector's own cap is what stops it there.
    #[test]
    fn the_collector_takes_no_byte_past_its_cap() {
        let mut collected = collector(4);
        assert!(matches!(collected.write(b"abc"), Ok(3)));
        assert!(matches!(collected.write(b"de"), Ok(0)));
        assert!(collected.over_limit);
        assert_eq!(collected.body, b"abc");
    }

    /// A download keeps up to its cap and stops the transfer there: memory stays bounded however
    /// much the host sends.
    #[test]
    fn a_download_collector_keeps_its_cap_and_stops_there() {
        let mut collected = Collector::new(4, true, Arc::new(AtomicBool::new(false)));
        assert!(matches!(collected.write(b"abc"), Ok(3)));
        assert!(matches!(collected.write(b"defgh"), Ok(1)));
        assert!(collected.over_limit);
        assert!(matches!(collected.write(b"ij"), Ok(0)));
        assert_eq!(collected.body, b"abcd");
    }

    #[test]
    fn a_download_request_carries_no_token_header_and_prints_no_address() {
        use super::super::download::LogDownloadUrl;
        let url = LogDownloadUrl::parse(
            "https://logs.example.com/job.txt?sig=c2lnbmF0dXJlMTIz",
            PlainHttp::Refused,
        )
        .unwrap();
        let mut request = DownloadRequest {
            op: "download the job log",
            url,
            limit: 2048,
            tail: false,
        };
        let headers = |request: &DownloadRequest| {
            let mut lines = Vec::new();
            for line in &download_headers(request).unwrap() {
                lines.push(String::from_utf8_lossy(line).into_owned());
            }
            lines
        };
        assert!(
            headers(&request).is_empty(),
            "the whole log asks for no range"
        );
        request.tail = true;
        assert_eq!(headers(&request), ["Range: bytes=-2048"]);
        let transfer = Transfer::Download(request.clone());
        assert_eq!(transfer.shown(), "https://logs.example.com/…");
        let scrubbed = server_text(
            &format!("failed on {} at {}", transfer.url(), transfer.shown()),
            &transfer.secrets(),
        );
        assert_eq!(
            scrubbed, "failed on <redacted> at https://logs.example.com/…",
            "curl's words lose the address"
        );
        let printed = format!("{request:?} {transfer:?}");
        assert!(printed.contains("logs.example.com"), "{printed}");
        assert!(!printed.contains("c2lnbmF0dXJlMTIz"), "{printed}");
        assert!(!printed.contains("job.txt"), "{printed}");
        let location = Location("https://x.example/?sig=c2lnbmF0dXJlMTIz".to_owned());
        assert!(!format!("{location:?}").contains("c2lnbmF0dXJlMTIz"));
    }

    #[test]
    fn the_collector_caps_headers_and_keeps_the_last_response_only() {
        let mut collected = collector(4);
        assert!(collected.header(b"HTTP/1.1 100 Continue\r\n"));
        assert!(collected.header(b"X-Early: 1\r\n"));
        assert!(collected.header(b"HTTP/1.1 200 OK\r\n"));
        assert!(collected.header(b"X-OAuth-Scopes:  repo \r\n"));
        assert_eq!(
            collected.headers,
            [("x-oauth-scopes".to_owned(), "repo".to_owned())]
        );
        let line = vec![b'a'; MAX_HEADER_BYTES];
        assert!(!collected.header(&line));
        assert!(collected.headers_over_limit);
    }

    #[test]
    fn a_raised_cancel_flag_aborts_at_the_next_progress_tick() {
        let mut collected = collector(4);
        assert!(collected.progress(0.0, 0.0, 0.0, 0.0));
        collected.cancel.store(true, Ordering::SeqCst);
        assert!(!collected.progress(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn requests_and_responses_never_print_a_secret() {
        let token = ApiToken::new("ghu_token_secret".to_owned()).unwrap();
        let request = Request {
            op: "refresh the token",
            method: Method::Post,
            url: super::super::GitHubHost::github_com().web_url("/login/oauth/access_token"),
            accept: "application/json",
            token: Some(token),
            api_version: false,
            body: Some(Body::Form(b"refresh_token=ghr_form_secret".to_vec())),
            limit: 10,
            scrub: vec!["ghr_scrub_secret".to_owned()],
        };
        let response = Response {
            status: 200,
            headers: vec![("set-cookie".to_owned(), "_gh_sess=cookie_secret".to_owned())],
            body: br#"{"access_token":"ghu_body_secret"}"#.to_vec(),
        };
        let json = Body::Json(br#"{"token":"json_secret"}"#.to_vec());
        let printed = format!("{request:?} {request:#?} {response:?} {response:#?} {json:?}");
        for secret in [
            "ghu_token_secret",
            "ghr_form_secret",
            "ghr_scrub_secret",
            "cookie_secret",
            "ghu_body_secret",
            "json_secret",
        ] {
            assert!(!printed.contains(secret), "{secret}: {printed}");
        }
        assert!(printed.contains("refresh the token") && printed.contains("set-cookie"));
    }

    /// A merge-shaped request to `url`, with no token.
    fn request_to(url: String) -> Request {
        Request {
            op: "merge the pull request",
            method: Method::Post,
            url: super::super::host::ApiUrl::for_test(url),
            accept: "application/json",
            token: None,
            api_version: false,
            body: Some(Body::Json(b"{\"q\":1}".to_vec())),
            limit: 1024,
            scrub: Vec::new(),
        }
    }

    /// A listener on a loopback port, and the port.
    fn listener() -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    /// Everything `stream` sends until it closes, or until `ends` is seen.
    fn read_from(stream: &mut std::net::TcpStream, ends: &[u8]) -> Vec<u8> {
        use std::io::Read as _;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !seen.ends_with(ends) {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => seen.extend_from_slice(&buffer[..read]),
            }
        }
        seen
    }

    #[test]
    fn a_refused_connection_sends_nothing() {
        let (listener, port) = listener();
        drop(listener);
        let delivery = deliver(
            request_to(format!("http://127.0.0.1:{port}/api/graphql")),
            &Cancel::new(),
        );
        assert!(
            matches!(delivery, Delivery::NotSent(GitHubError::Transport { .. })),
            "{delivery:?}"
        );
    }

    #[test]
    fn a_connection_that_ends_after_the_request_arrived_may_have_sent_it() {
        for reset in [false, true] {
            let (listener, port) = listener();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                if reset {
                    use std::io::Read as _;
                    let mut first = [0_u8; 4];
                    stream.read_exact(&mut first).unwrap();
                    // Closing with the rest of the request unread makes the kernel reset it.
                    return first.to_vec();
                }
                read_from(&mut stream, b"{\"q\":1}")
            });
            let delivery = deliver(
                request_to(format!("http://127.0.0.1:{port}/api/graphql")),
                &Cancel::new(),
            );
            let seen = server.join().unwrap();
            assert!(seen.starts_with(b"POST"), "{reset}: {seen:?}");
            assert!(
                matches!(delivery, Delivery::Sent(Err(GitHubError::Transport { .. }))),
                "{reset}: {delivery:?}"
            );
        }
    }

    #[test]
    fn a_failed_tls_handshake_sends_no_byte_of_the_request() {
        use std::io::Write as _;
        let (listener, port) = listener();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
            read_from(&mut stream, b"\0never")
        });
        let delivery = deliver(
            request_to(format!("https://127.0.0.1:{port}/api/graphql")),
            &Cancel::new(),
        );
        let seen = server.join().unwrap();
        assert!(
            !String::from_utf8_lossy(&seen).contains("POST"),
            "the request never left: {seen:?}"
        );
        assert!(
            matches!(delivery, Delivery::NotSent(GitHubError::Transport { .. })),
            "{delivery:?}"
        );
    }

    #[test]
    fn the_dev_allowance_reaches_loopback_only() {
        assert!(allowed_url("http://127.0.0.1:4010/x", PlainHttp::Loopback));
        assert!(allowed_url("http://localhost:4010/x", PlainHttp::Loopback));
        for url in [
            "http://example.com/x",
            "http://127.0.0.1.evil.example/x",
            "http://localhost.evil.example/x",
            "http://127.0.0.1@evil.example/x",
        ] {
            assert!(!allowed_url(url, PlainHttp::Loopback), "{url}");
        }
    }
}
