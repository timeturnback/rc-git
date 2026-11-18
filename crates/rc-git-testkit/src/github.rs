//! A fake GitHub host for tests: the device-flow and token endpoints, `GET /user`, a GraphQL
//! echo, and endpoints that misbehave on purpose (a redirect, an oversized body, a stall, an
//! error that echoes the token, a token endpoint that echoes the form it got). Plain HTTP on
//! loopback, laid out as a GitHub Enterprise host
//! (`/api/v3`, `/api/graphql`), since rc-git builds github.com's URLs for `api.github.com`.
//!
//! It is strict: every endpoint answers only for the exact method, headers and parameters a
//! correct client sends, and anything else gets an error, so a client that sends the wrong
//! thing fails its test instead of passing against a lenient fake. Every request is recorded,
//! headers included.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// The API version a correct client asks for.
pub const API_VERSION: &str = "2022-11-28";

/// The device code the fake hands out.
pub const DEVICE_CODE: &str = "fake-device-code-0123456789";

/// The user code the fake hands out.
pub const USER_CODE: &str = "WDJB-MJHT";

/// One request as the server read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// `GET` or `POST`.
    pub method: String,
    /// The request target: path and query.
    pub target: String,
    /// Every header, names lowercase, in arrival order.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The first value of header `name` (lowercase).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A token the fake knows, and what `GET /user` says about it.
#[derive(Debug, Clone)]
pub struct FakeToken {
    /// The token's text.
    pub token: String,
    /// The login it signs in as.
    pub login: String,
    /// Sent as `X-OAuth-Scopes` when set.
    pub scopes: Option<String>,
    /// Sent as `GitHub-Authentication-Token-Expiration` when set.
    pub expires: Option<String>,
}

/// What one device-flow poll answers.
#[derive(Debug, Clone)]
pub enum PollAnswer {
    /// `authorization_pending`.
    Pending,
    /// `slow_down`, naming `interval` seconds.
    SlowDown {
        /// The interval the answer names.
        interval: u64,
    },
    /// `access_denied`.
    Denied,
    /// `expired_token`.
    Expired,
    /// The tokens.
    Grant(Grant),
}

/// Tokens the token endpoint hands out.
#[derive(Debug, Clone)]
pub struct Grant {
    /// The access token.
    pub access: String,
    /// `expires_in`, when set.
    pub expires_in: Option<u64>,
    /// The refresh token, when set.
    pub refresh: Option<String>,
}

/// What the device-code endpoint answers.
#[derive(Debug, Clone)]
pub struct DeviceScript {
    /// The client id it answers for.
    pub client_id: String,
    /// The `scope` it expects: `Some("repo")` for an OAuth App, `None` for a GitHub App.
    pub scope: Option<String>,
    /// The `verification_uri` it names; the server's own `/login/device` when `None`.
    pub verification_uri: Option<String>,
    /// `interval` in seconds.
    pub interval: u64,
    /// `expires_in` in seconds.
    pub expires_in: u64,
    /// The poll answers, in order; when they run out, polls answer `authorization_pending`.
    pub polls: VecDeque<PollAnswer>,
}

/// Where the token endpoint echoes the form values it received (a device code, a refresh
/// token), as a careless or hostile host would, so a test can check that none reaches an
/// error. The values are joined by spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoForm {
    /// In `error_description`, under the error code `echoed_form`.
    Description,
    /// As the `error` code itself.
    Code,
    /// As a `400` body that is not JSON.
    Body,
}

#[derive(Debug, Default)]
struct State {
    device: Option<DeviceScript>,
    tokens: Vec<FakeToken>,
    /// Refresh token to the grant it buys, with the client id it is for.
    refreshes: BTreeMap<String, (String, Grant)>,
    used_refreshes: BTreeSet<String>,
    redirect_to: Option<String>,
    oversized_bytes: usize,
    echo_form: Option<EchoForm>,
    requests: Vec<RecordedRequest>,
}

/// A fake GitHub host on `127.0.0.1`, answering from background threads until dropped.
#[derive(Debug)]
pub struct GitHubServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    handle: Option<JoinHandle<()>>,
}

impl GitHubServer {
    /// A server that knows no client, token or refresh token yet.
    ///
    /// # Panics
    /// When the listener cannot bind.
    #[must_use]
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State {
            oversized_bytes: 2 * 1024 * 1024,
            ..State::default()
        }));
        let (stop, shared) = (Arc::clone(&shutdown), Arc::clone(&state));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
                std::thread::spawn(move || serve(stream, port, &shared, &stop));
            }
        });
        Self {
            port,
            shutdown,
            state,
            handle: Some(handle),
        }
    }

    /// The host as a user types it in Settings: `127.0.0.1:<port>`.
    #[must_use]
    pub fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn origin(&self) -> String {
        format!("http://{}", self.host())
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Answers the device-code endpoint and its polls from `script`.
    pub fn script_device_flow(&self, script: DeviceScript) {
        self.state().device = Some(script);
    }

    /// Knows `token` from now on.
    pub fn add_token(&self, token: FakeToken) {
        self.state().tokens.push(token);
    }

    /// Accepts `refresh` once, from `client_id`, buying `grant`.
    pub fn add_refresh(&self, refresh: &str, client_id: &str, grant: Grant) {
        self.state()
            .refreshes
            .insert(refresh.to_owned(), (client_id.to_owned(), grant));
    }

    /// Where `GET /api/v3/redirect` sends the client.
    pub fn redirect_to(&self, location: &str) {
        self.state().redirect_to = Some(location.to_owned());
    }

    /// How many bytes `GET /api/v3/oversized` answers with (2 MiB unless set).
    pub fn set_oversized_bytes(&self, bytes: usize) {
        self.state().oversized_bytes = bytes;
    }

    /// From now on the token endpoint answers every well-formed form (a device-flow poll, a
    /// refresh) with its values echoed `into` an error, instead of judging it.
    pub fn echo_form(&self, into: EchoForm) {
        self.state().echo_form = Some(into);
    }

    /// Every request so far, in arrival order.
    #[must_use]
    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.state().requests.clone()
    }

    /// How many requests arrived so far.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.state().requests.len()
    }

    /// How many requests to `path` (the target before any `?`) arrived so far.
    #[must_use]
    pub fn requests_to(&self, path: &str) -> usize {
        self.state()
            .requests
            .iter()
            .filter(|request| request.target.split('?').next() == Some(path))
            .count()
    }
}

impl Drop for GitHubServer {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test kit opens a loopback connection to stop its own server"
    )]
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        drop(TcpStream::connect(("127.0.0.1", self.port)));
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One HTTP answer.
struct Answer {
    status: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    /// Send the body without `Content-Length`, ending at close.
    streamed: bool,
}

fn json(status: &'static str, body: String) -> Answer {
    Answer {
        status,
        headers: Vec::new(),
        body: body.into_bytes(),
        streamed: false,
    }
}

fn not_found() -> Answer {
    json("404 Not Found", r#"{"message":"Not Found"}"#.to_owned())
}

fn bad_request(why: &str) -> Answer {
    json(
        "400 Bad Request",
        format!(r#"{{"message":"{}"}}"#, escape(why)),
    )
}

/// `text` escaped for a JSON string.
fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                write!(out, "\\u{:04x}", u32::from(c)).expect("write to a String");
            }
            c => out.push(c),
        }
    }
    out
}

fn serve(stream: TcpStream, port: u16, state: &Mutex<State>, stop: &AtomicBool) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    lock(state).requests.push(request.clone());
    let path = request
        .target
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let answer = match (request.method.as_str(), path.as_str()) {
        ("POST", "/login/device/code") => device_code(&request, port, state),
        ("POST", "/login/oauth/access_token") => token_endpoint(&request, state),
        ("GET", "/api/v3/user") => user(&request, state),
        ("GET", "/api/v3/redirect") => redirect(state),
        ("GET", "/api/v3/oversized") => oversized(state, false),
        ("GET", "/api/v3/oversized-unsized") => oversized(state, true),
        ("GET", "/api/v3/echo-error") => echo_error(&request),
        ("GET", "/api/v3/rate-limited") => rate_limited(),
        ("GET", "/api/v3/stall") => {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            return;
        }
        ("POST", "/api/graphql") => graphql(&request, state),
        (method, target) if method == "GET" && target.starts_with("/api/v3/repos/") => {
            repository(&request, target, state)
        }
        (_, _) => not_found(),
    };
    let mut writer = stream;
    write_answer(&mut writer, &answer);
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<RecordedRequest> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = Vec::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name == "content-length" {
                length = value.trim().parse().ok()?;
            }
            headers.push((name, value.trim().to_owned()));
        }
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(RecordedRequest {
        method,
        target,
        headers,
        body,
    })
}

fn write_answer(writer: &mut TcpStream, answer: &Answer) {
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\n",
        answer.status
    );
    if !answer.streamed {
        write!(head, "Content-Length: {}\r\n", answer.body.len()).expect("write to a String");
    }
    for (name, value) in &answer.headers {
        write!(head, "{name}: {value}\r\n").expect("write to a String");
    }
    head.push_str("Connection: close\r\n\r\n");
    drop(writer.write_all(head.as_bytes()));
    drop(writer.write_all(&answer.body));
    drop(writer.flush());
}

/// The form body as key-value pairs, decoded; `None` when it is not a form or repeats a key.
fn form(request: &RecordedRequest) -> Option<BTreeMap<String, String>> {
    if request.header("content-type") != Some("application/x-www-form-urlencoded") {
        return None;
    }
    let text = String::from_utf8(request.body.clone()).ok()?;
    let mut pairs = BTreeMap::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=')?;
        if pairs.insert(decode(name)?, decode(value)?).is_some() {
            return None;
        }
    }
    Some(pairs)
}

fn decode(text: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut iter = text.bytes();
    while let Some(byte) = iter.next() {
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => {
                let hex = [iter.next()?, iter.next()?];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes).ok()
}

/// Whether the request asks for JSON, as the OAuth endpoints need.
fn wants_json(request: &RecordedRequest) -> bool {
    request.header("accept") == Some("application/json")
}

fn device_code(request: &RecordedRequest, port: u16, state: &Mutex<State>) -> Answer {
    let Some(fields) = form(request).filter(|_| wants_json(request)) else {
        return bad_request("send a form and Accept: application/json");
    };
    let state = lock(state);
    let Some(script) = &state.device else {
        return not_found();
    };
    let mut expected = BTreeMap::from([("client_id".to_owned(), script.client_id.clone())]);
    if let Some(scope) = &script.scope {
        expected.insert("scope".to_owned(), scope.clone());
    }
    if fields != expected {
        return oauth_error("incorrect_client_credentials");
    }
    let uri = script
        .verification_uri
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{port}/login/device"));
    let body = [
        format!(r#"{{"device_code":"{DEVICE_CODE}","user_code":"{USER_CODE}","#),
        format!(r#""verification_uri":"{}","#, escape(&uri)),
        format!(
            r#""expires_in":{},"interval":{}}}"#,
            script.expires_in, script.interval
        ),
    ];
    json("200 OK", body.concat())
}

fn oauth_error(code: &str) -> Answer {
    json(
        "200 OK",
        format!(r#"{{"error":"{code}","error_description":"{code} from the fake"}}"#),
    )
}

fn grant_body(grant: &Grant) -> String {
    let access = escape(&grant.access);
    let mut body = format!(r#"{{"access_token":"{access}","token_type":"bearer","scope":"""#);
    if let Some(expires) = grant.expires_in {
        write!(body, r#","expires_in":{expires}"#).expect("write to a String");
    }
    if let Some(refresh) = &grant.refresh {
        let refresh = escape(refresh);
        write!(
            body,
            r#","refresh_token":"{refresh}","refresh_token_expires_in":15897600"#
        )
        .expect("write to a String");
    }
    body.push('}');
    body
}

fn token_endpoint(request: &RecordedRequest, state: &Mutex<State>) -> Answer {
    let Some(fields) = form(request).filter(|_| wants_json(request)) else {
        return bad_request("send a form and Accept: application/json");
    };
    if fields.contains_key("client_secret") {
        return bad_request("rc-git has no client secret and must never send one");
    }
    let echo = lock(state).echo_form;
    if let Some(into) = echo {
        return echoed(&fields, into);
    }
    match fields.get("grant_type").map(String::as_str) {
        Some("urn:ietf:params:oauth:grant-type:device_code") => device_poll(&fields, state),
        Some("refresh_token") => refresh(&fields, state),
        Some(_) | None => oauth_error("unsupported_grant_type"),
    }
}

/// The form's values joined by spaces, echoed `into` an error answer.
fn echoed(fields: &BTreeMap<String, String>, into: EchoForm) -> Answer {
    let values: Vec<&str> = fields.values().map(String::as_str).collect();
    let sent = escape(&values.join(" "));
    match into {
        EchoForm::Description => json(
            "200 OK",
            format!(r#"{{"error":"echoed_form","error_description":"you sent {sent}"}}"#),
        ),
        EchoForm::Code => json(
            "200 OK",
            format!(r#"{{"error":"{sent}","error_description":"echoed"}}"#),
        ),
        EchoForm::Body => Answer {
            status: "400 Bad Request",
            headers: Vec::new(),
            body: format!("you sent {sent}").into_bytes(),
            streamed: false,
        },
    }
}

fn device_poll(fields: &BTreeMap<String, String>, state: &Mutex<State>) -> Answer {
    let mut state = lock(state);
    let Some(script) = state.device.as_mut() else {
        return not_found();
    };
    let expected = BTreeSet::from(["client_id", "device_code", "grant_type"]);
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected
        || fields.get("client_id") != Some(&script.client_id)
        || fields.get("device_code").map(String::as_str) != Some(DEVICE_CODE)
    {
        return oauth_error("incorrect_device_code");
    }
    match script.polls.pop_front().unwrap_or(PollAnswer::Pending) {
        PollAnswer::Pending => oauth_error("authorization_pending"),
        PollAnswer::SlowDown { interval } => json(
            "200 OK",
            format!(r#"{{"error":"slow_down","error_description":"slow","interval":{interval}}}"#),
        ),
        PollAnswer::Denied => oauth_error("access_denied"),
        PollAnswer::Expired => oauth_error("expired_token"),
        PollAnswer::Grant(grant) => json("200 OK", grant_body(&grant)),
    }
}

fn refresh(fields: &BTreeMap<String, String>, state: &Mutex<State>) -> Answer {
    let mut state = lock(state);
    let expected = BTreeSet::from(["client_id", "grant_type", "refresh_token"]);
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return bad_request("a refresh sends client_id, grant_type and refresh_token only");
    }
    let token = fields.get("refresh_token").cloned().unwrap_or_default();
    let Some((client_id, grant)) = state.refreshes.get(&token).cloned() else {
        return oauth_error("bad_refresh_token");
    };
    if fields.get("client_id") != Some(&client_id) || !state.used_refreshes.insert(token) {
        return oauth_error("bad_refresh_token");
    }
    json("200 OK", grant_body(&grant))
}

/// The known token a correct API request carries, `None` when the headers are not exactly
/// what rc-git must send.
fn authorized(request: &RecordedRequest, state: &State, accept: &str) -> Option<FakeToken> {
    if request.header("accept") != Some(accept)
        || request.header("x-github-api-version") != Some(API_VERSION)
        || request.header("user-agent").is_none_or(str::is_empty)
    {
        return None;
    }
    let token = request.header("authorization")?.strip_prefix("Bearer ")?;
    state
        .tokens
        .iter()
        .find(|known| known.token == token)
        .cloned()
}

fn unauthorized() -> Answer {
    json(
        "401 Unauthorized",
        r#"{"message":"Bad credentials"}"#.to_owned(),
    )
}

fn user(request: &RecordedRequest, state: &Mutex<State>) -> Answer {
    let state = lock(state);
    let Some(token) = authorized(request, &state, "application/vnd.github+json") else {
        return unauthorized();
    };
    let mut answer = json(
        "200 OK",
        format!(r#"{{"login":"{}","id":1}}"#, escape(&token.login)),
    );
    if let Some(scopes) = token.scopes {
        answer.headers.push(("X-OAuth-Scopes".to_owned(), scopes));
    }
    if let Some(expires) = token.expires {
        answer
            .headers
            .push(("GitHub-Authentication-Token-Expiration".to_owned(), expires));
    }
    answer
}

fn redirect(state: &Mutex<State>) -> Answer {
    let Some(location) = lock(state).redirect_to.clone() else {
        return not_found();
    };
    Answer {
        status: "302 Found",
        headers: vec![("Location".to_owned(), location)],
        body: Vec::new(),
        streamed: false,
    }
}

fn oversized(state: &Mutex<State>, streamed: bool) -> Answer {
    let bytes = lock(state).oversized_bytes;
    Answer {
        status: "200 OK",
        headers: Vec::new(),
        body: vec![b' '; bytes],
        streamed,
    }
}

fn echo_error(request: &RecordedRequest) -> Answer {
    let sent = request.header("authorization").unwrap_or_default();
    json(
        "422 Unprocessable Entity",
        format!(r#"{{"message":"you sent {}"}}"#, escape(sent)),
    )
}

fn rate_limited() -> Answer {
    let mut answer = json(
        "403 Forbidden",
        r#"{"message":"API rate limit exceeded"}"#.to_owned(),
    );
    answer
        .headers
        .push(("X-RateLimit-Remaining".to_owned(), "0".to_owned()));
    answer
        .headers
        .push(("X-RateLimit-Reset".to_owned(), "1700000000".to_owned()));
    answer
}

fn graphql(request: &RecordedRequest, state: &Mutex<State>) -> Answer {
    let state = lock(state);
    if authorized(request, &state, "application/vnd.github+json").is_none() {
        return unauthorized();
    }
    if request.header("content-type") != Some("application/json") {
        return bad_request("GraphQL takes a JSON body");
    }
    let Ok(body) = String::from_utf8(request.body.clone()) else {
        return bad_request("the body is not UTF-8");
    };
    json("200 OK", format!(r#"{{"data":{{"echo":{body}}}}}"#))
}

fn repository(request: &RecordedRequest, target: &str, state: &Mutex<State>) -> Answer {
    let state = lock(state);
    if authorized(request, &state, "application/vnd.github+json").is_none() {
        return unauthorized();
    }
    let full_name = target.trim_start_matches("/api/v3/repos/");
    json(
        "200 OK",
        format!(r#"{{"full_name":"{}"}}"#, escape(full_name)),
    )
}
