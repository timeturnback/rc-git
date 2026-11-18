//! The one HTTP client in rc-git that talks to an API, on libcurl. Every request goes through
//! [`send`], which fixes the transport rules in one place:
//!
//! - the URL is an [`ApiUrl`] (built from the account's host only) and must be `https://`, or
//!   `http://` to a loopback host under the dev allowance; curl's protocol list cannot be set
//!   through the safe `curl` API, so this check is the restriction, and with redirects off the
//!   request URL is the only one curl ever connects to;
//! - redirects are never followed, and a 3xx answer is an error;
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

/// Whether a request may have reached the host, for a mutation, whose failure means one thing
/// when nothing left the machine and another when the host may have acted on it.
#[derive(Debug)]
pub(super) enum Delivery {
    /// Nothing left the machine: a cancel before the transfer started, a URL the transport
    /// refuses, or a failure to resolve, connect or finish the TLS handshake, all of which come
    /// before curl writes a byte of the request.
    NotSent(GitHubError),
    /// The request may have reached the host, and this came back: the response, or why none
    /// arrived (a timeout, a dropped connection, a cancel while it ran, a redirect, a body over
    /// its cap).
    Sent(GitHubResult<Response>),
}

/// [`send`], telling apart a failure before the request left from one after. Every transport
/// rule is the same, since `send` is this function. A failure it cannot place (the request
/// thread did not start, or ended without a word) counts as sent: a mutation then reads what
/// happened instead of assuming nothing did.
pub(super) fn deliver(request: Request, cancel: &Cancel) -> Delivery {
    let op = request.op;
    if cancel.is_cancelled() {
        return Delivery::NotSent(GitHubError::Cancelled { op });
    }
    if !allowed_url(request.url.as_str(), PLAIN_HTTP) {
        return Delivery::NotSent(GitHubError::Transport {
            op,
            url: request.url.as_str().to_owned(),
            reason: "rc-git speaks only https:// to GitHub".to_owned(),
        });
    }
    let worker_cancel = cancel.clone();
    match detached(cancel, op, move || Ok(perform(&request, &worker_cancel))) {
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
    over_limit: bool,
    headers: Vec<(String, String)>,
    header_bytes: usize,
    headers_over_limit: bool,
    cancel: Arc<AtomicBool>,
}

impl Handler for Collector {
    fn write(&mut self, data: &[u8]) -> Result<usize, WriteError> {
        if self.body.len().saturating_add(data.len()) > self.limit {
            self.over_limit = true;
            // Taking fewer bytes than offered makes curl abort the transfer.
            return Ok(0);
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

/// The header list for `request`.
fn headers(request: &Request) -> Result<List, curl::Error> {
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

/// Builds the handle for `request`. Every option that bounds or restricts the transfer is set
/// here, so no request can leave without them.
fn configure(request: &Request, cancel: &Cancel) -> Result<Easy2<Collector>, curl::Error> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the GitHub client is the one sanctioned curl user; see docs/safety.md"
    )]
    let mut easy = Easy2::new(Collector {
        body: Vec::new(),
        limit: request.limit,
        over_limit: false,
        headers: Vec::new(),
        header_bytes: 0,
        headers_over_limit: false,
        cancel: cancel.share(),
    });
    easy.url(request.url.as_str())?;
    easy.follow_location(false)?;
    easy.netrc(NetRc::Ignored)?;
    easy.useragent(USER_AGENT)?;
    easy.connect_timeout(cancel.connect())?;
    easy.low_speed_limit(1)?;
    easy.low_speed_time(cancel.idle().max(std::time::Duration::from_secs(1)))?;
    easy.timeout(cancel.connect() + cancel.idle())?;
    easy.max_filesize(u64::try_from(request.limit).unwrap_or(u64::MAX))?;
    easy.progress(true)?;
    if request.url.as_str().starts_with("http://") {
        // A loopback test server: no environment proxy may stand in between.
        easy.proxy("")?;
    }
    easy.http_headers(headers(request)?)?;
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
fn perform(request: &Request, cancel: &Cancel) -> Delivery {
    let op = request.op;
    let url = request.url.as_str().to_owned();
    let transport = |error: &curl::Error| GitHubError::Transport {
        op,
        url: url.clone(),
        reason: server_text(&describe(error), &secrets(request)),
    };
    let easy = match configure(request, cancel) {
        Ok(easy) => easy,
        Err(error) => return Delivery::NotSent(transport(&error)),
    };
    let result = easy.perform();
    let collected = easy.get_ref();
    if collected.over_limit
        || result
            .as_ref()
            .is_err_and(curl::Error::is_filesize_exceeded)
    {
        return Delivery::Sent(Err(GitHubError::TooLarge {
            op,
            url,
            limit: request.limit,
        }));
    }
    if let Err(error) = result {
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
        if before_request(&error) {
            return Delivery::NotSent(transport(&error));
        }
        return Delivery::Sent(Err(transport(&error)));
    }
    let status = match easy.response_code() {
        Ok(status) => status,
        Err(error) => return Delivery::Sent(Err(transport(&error))),
    };
    if (300..400).contains(&status) {
        return Delivery::Sent(Err(GitHubError::Redirected { op, url, status }));
    }
    let collected = easy.get_ref();
    Delivery::Sent(Ok(Response {
        status,
        headers: collected.headers.clone(),
        body: collected.body.clone(),
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
        Collector {
            body: Vec::new(),
            limit,
            over_limit: false,
            headers: Vec::new(),
            header_bytes: 0,
            headers_over_limit: false,
            cancel: Arc::new(AtomicBool::new(false)),
        }
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
