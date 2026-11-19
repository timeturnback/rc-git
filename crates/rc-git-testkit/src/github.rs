//! A fake GitHub host for tests: the device-flow and token endpoints, `GET /user`, scripted
//! GraphQL answers (or an echo), and endpoints that misbehave on purpose (a redirect, an
//! oversized body, a stall, an error that echoes the token, a token endpoint that echoes the
//! form it got). Plain HTTP on loopback, laid out as a GitHub Enterprise host
//! (`/api/v3`, `/api/graphql`), since rc-git builds github.com's URLs for `api.github.com`.
//! Its git smart-HTTP endpoint (`<owner>/<name>.git/info/refs`) answers every request with a
//! `401` Basic challenge, as GitHub does for a private repository, so a test sees exactly what
//! credentials a fetch offered the git transport.
//!
//! It is strict: every endpoint answers only for the exact method, headers and parameters a
//! correct client sends, and a GraphQL query only for the exact request body scripted for it;
//! anything else gets an error, so a client that sends the wrong thing fails its test instead
//! of passing against a lenient fake. Every request is recorded, headers included, and the
//! ones that could change something are counted apart from the reads.
//!
//! One pull request can be scripted to merge ([`MergeScript`]): its read answers from its state
//! (open, moved, merged), and its one merge mutation succeeds, is refused, answers with words
//! and headers set apart from what it did, meets a head that moved after a given read, meets a
//! pull request that changed some other way after a given read (a check that finished, a merge
//! state that cleared), or merges and then stalls or drops the connection, or drops it without
//! merging. The admin merge sends the plain merge's exact
//! mutation, so the same script serves both: what differs is the read (`mergeStateStatus`,
//! `viewerCanMergeAsAdmin`). The same script serves turning auto-merge on or off too: its one
//! mutation is then that change, and `merged` is the read once the change applied.
//!
//! The re-run of a workflow run's failed jobs (`POST .../actions/runs/<id>/rerun-failed-jobs`)
//! answers per run as scripted ([`RerunBehaviour`]): re-run with `201 Created`, an answer that
//! re-runs nothing, re-run then drop the connection, or drop it without re-running. Each kind of
//! mutation is counted apart ([`GitHubServer::mutations_named`],
//! [`GitHubServer::rerun_requests`]).
//!
//! A job log is served as GitHub serves it: the job's read (`GET .../actions/jobs/<id>`), the logs
//! endpoint (`GET .../actions/jobs/<id>/logs`) answering a scripted redirect ([`LogsAnswer`]), and
//! a [`StorageServer`], a second loopback listener standing in for the host the redirect names,
//! which records every request's headers and answers as scripted ([`StorageBehaviour`]): the log
//! with or without honouring a suffix `Range`, a body without a length, any status (a redirect of
//! its own included), or a stall. A workflow run's read (`GET .../actions/runs/<id>`) answers as
//! scripted too. An unscripted job, log or run read is a `404`.

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

/// One pull request the fake can merge: the read that shows it, what that read answers before
/// and after, the one merge mutation it takes, and how that mutation behaves. The read answers
/// from the pull request's state, so a test sees what a merge changed exactly as a client
/// reading it again would.
#[derive(Debug, Clone)]
pub struct MergeScript {
    /// The exact body of the read that shows the pull request.
    pub read: String,
    /// The read's answer while it is open at its first head.
    pub open: String,
    /// The read's answer once its head moved ([`MergeBehaviour::MoveHeadAfter`]), or once it
    /// changed ([`MergeBehaviour::ChangeAfter`]).
    pub moved: String,
    /// The read's answer once it merged.
    pub merged: String,
    /// The exact body of the merge mutation; any other mutation is refused with a 400.
    pub mutation: String,
    /// The mutation's answer when it merges.
    pub reply: String,
    /// What the mutation does.
    pub behaviour: MergeBehaviour,
}

/// What the fake does with the merge mutation.
#[derive(Debug, Clone)]
pub enum MergeBehaviour {
    /// Merges and answers [`MergeScript::reply`].
    Merge,
    /// Answers `status` with `body` and merges nothing.
    Refuse {
        /// The status line, e.g. `200 OK` or `422 Unprocessable Entity`.
        status: &'static str,
        /// The body.
        body: String,
    },
    /// Merges when `merges` says so, then answers `200 OK` with `body` and the extra
    /// `headers` (a spent rate limit, say): an answer whose words and whose effect a test sets
    /// apart.
    Answer {
        /// Whether the pull request merges before the answer.
        merges: bool,
        /// The body.
        body: String,
        /// Extra response headers, as `(name, value)`.
        headers: Vec<(String, String)>,
    },
    /// The head moves once the read has answered `reads` times: later reads answer
    /// [`MergeScript::moved`], and the mutation is refused with `refusal` (a `200 OK` body), as
    /// GitHub refuses a merge whose expected head is no longer the head.
    MoveHeadAfter {
        /// Reads answered before the head moves.
        reads: usize,
        /// The mutation's answer once it moved.
        refusal: String,
    },
    /// The pull request changes once the read has answered `reads` times: later reads answer
    /// [`MergeScript::moved`] (the same head, with a check that moved, a merge state that
    /// cleared, an ability that was withdrawn), and the mutation, should one still arrive,
    /// merges as [`MergeBehaviour::Merge`] does, so a client that sends it anyway is seen.
    ChangeAfter {
        /// Reads answered before it changes.
        reads: usize,
    },
    /// Merges, then holds the connection open without answering until the client gives up.
    MergeThenStall,
    /// Merges, then closes the connection without answering.
    MergeThenDrop,
    /// Closes the connection without answering and merges nothing.
    Drop,
}

/// Where the scripted pull request is.
#[derive(Debug)]
struct MergeState {
    script: MergeScript,
    reads: usize,
    merged: bool,
}

impl MergeState {
    fn moved(&self) -> bool {
        match self.script.behaviour {
            MergeBehaviour::MoveHeadAfter { reads, .. } | MergeBehaviour::ChangeAfter { reads } => {
                self.reads >= reads
            }
            MergeBehaviour::Merge
            | MergeBehaviour::Refuse { .. }
            | MergeBehaviour::Answer { .. }
            | MergeBehaviour::MergeThenStall
            | MergeBehaviour::MergeThenDrop
            | MergeBehaviour::Drop => false,
        }
    }
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
    /// Exact GraphQL request bodies and what each is answered with.
    graphql: Vec<(String, Scripted)>,
    /// Whether an unscripted GraphQL request is echoed back instead of refused.
    graphql_echo: bool,
    /// The pull request a merge acts on, when scripted.
    merge: Option<MergeState>,
    /// The workflow runs a re-run of failed jobs is scripted for, by `owner/name` and run id.
    reruns: BTreeMap<(String, u64), RerunBehaviour>,
    /// How many re-runs each scripted run took, by run id.
    reruns_applied: BTreeMap<u64, usize>,
    /// The REST reads of jobs and workflow runs, by their exact path under `/api/v3`: the status
    /// line and the body.
    actions: BTreeMap<String, (&'static str, String)>,
    /// The logs endpoints' answers, by their exact path under `/api/v3`.
    logs: BTreeMap<String, LogsAnswer>,
    requests: Vec<RecordedRequest>,
}

/// What the fake's logs endpoint (`GET .../actions/jobs/<id>/logs`) answers.
#[derive(Debug, Clone)]
pub enum LogsAnswer {
    /// A redirect: `status` (e.g. `302 Found`) with `Location: location` and no body.
    Redirect {
        /// The status line.
        status: &'static str,
        /// The `Location` header, sent exactly as given.
        location: String,
    },
    /// `status` with `body`, no `Location`.
    Answer {
        /// The status line.
        status: &'static str,
        /// The body.
        body: String,
    },
}

/// The exact body a correct re-run of failed jobs sends.
pub const RERUN_BODY: &str = r#"{"enable_debug_logging":false}"#;

/// What the fake does with a re-run of one workflow run's failed jobs.
#[derive(Debug, Clone)]
pub enum RerunBehaviour {
    /// Re-runs it and answers `201 Created` with `{}`.
    Rerun,
    /// Answers `status` with `body` and re-runs nothing (a refusal, or an answer with a status
    /// GitHub does not document for it).
    Answer {
        /// The status line, e.g. `404 Not Found`.
        status: &'static str,
        /// The body.
        body: String,
        /// Extra response headers, as `(name, value)` (a spent rate limit, say).
        headers: Vec<(String, String)>,
    },
    /// Re-runs it, then closes the connection without answering.
    RerunThenDrop,
    /// Closes the connection without answering and re-runs nothing.
    Drop,
}

/// A scripted GraphQL answer: a `200 OK` with this body and these extra headers.
#[derive(Debug, Clone)]
struct Scripted {
    body: String,
    headers: Vec<(String, String)>,
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

    /// Answers a GraphQL request whose body is exactly `request_body` with `answer` (a JSON
    /// document) from now on. Any other GraphQL body is refused with a 400, unless
    /// [`GitHubServer::echo_graphql`] is on.
    pub fn script_graphql(&self, request_body: &str, answer: &str) {
        self.script_graphql_with_headers(request_body, answer, &[]);
    }

    /// As [`GitHubServer::script_graphql`], with extra response headers (a spent rate limit).
    pub fn script_graphql_with_headers(
        &self,
        request_body: &str,
        answer: &str,
        headers: &[(&str, &str)],
    ) {
        let scripted = Scripted {
            body: answer.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        };
        let mut state = self.state();
        state.graphql.retain(|(body, _)| body != request_body);
        state.graphql.push((request_body.to_owned(), scripted));
    }

    /// Answers every unscripted GraphQL request with `{"data":{"echo":<the request body>}}`.
    pub fn echo_graphql(&self) {
        self.state().graphql_echo = true;
    }

    /// Serves the pull request `script` describes from now on: its read ahead of any
    /// [`GitHubServer::script_graphql`] answer for the same body, and its merge mutation.
    pub fn script_merge(&self, script: MergeScript) {
        self.state().merge = Some(MergeState {
            script,
            reads: 0,
            merged: false,
        });
    }

    /// Answers a re-run of the failed jobs of workflow run `run` of `repository` (`owner/name`)
    /// as `behaviour` says from now on. A re-run of any run not scripted is a `404`.
    pub fn script_rerun(&self, repository: &str, run: u64, behaviour: RerunBehaviour) {
        self.state()
            .reruns
            .insert((repository.to_owned(), run), behaviour);
    }

    /// How many requests arrived at run `run`'s re-run path of `repository`, whatever came of
    /// them.
    #[must_use]
    pub fn rerun_requests(&self, repository: &str, run: u64) -> usize {
        self.requests_to(&format!(
            "/api/v3/repos/{repository}/actions/runs/{run}/rerun-failed-jobs"
        ))
    }

    /// Answers the read of job `job` of `repository` (`owner/name`) with `status` and `body`
    /// (a JSON document) from now on.
    pub fn script_job(&self, repository: &str, job: u64, status: &'static str, body: &str) {
        self.state().actions.insert(
            format!("/repos/{repository}/actions/jobs/{job}"),
            (status, body.to_owned()),
        );
    }

    /// Answers the logs endpoint of job `job` of `repository` as `answer` says from now on.
    pub fn script_job_logs(&self, repository: &str, job: u64, answer: LogsAnswer) {
        self.state().logs.insert(
            format!("/repos/{repository}/actions/jobs/{job}/logs"),
            answer,
        );
    }

    /// Answers the read of workflow run `run` of `repository` with `status` and `body` from now
    /// on.
    pub fn script_workflow_run(
        &self,
        repository: &str,
        run: u64,
        status: &'static str,
        body: &str,
    ) {
        self.state().actions.insert(
            format!("/repos/{repository}/actions/runs/{run}"),
            (status, body.to_owned()),
        );
    }

    /// How many requests arrived at job `job`'s read of `repository`.
    #[must_use]
    pub fn job_reads(&self, repository: &str, job: u64) -> usize {
        self.requests_to(&format!("/api/v3/repos/{repository}/actions/jobs/{job}"))
    }

    /// How many requests arrived at job `job`'s logs endpoint of `repository`.
    #[must_use]
    pub fn log_requests(&self, repository: &str, job: u64) -> usize {
        self.requests_to(&format!(
            "/api/v3/repos/{repository}/actions/jobs/{job}/logs"
        ))
    }

    /// How many requests arrived at workflow run `run`'s read of `repository`.
    #[must_use]
    pub fn run_reads(&self, repository: &str, run: u64) -> usize {
        self.requests_to(&format!("/api/v3/repos/{repository}/actions/runs/{run}"))
    }

    /// How many re-runs of run `run` the fake carried out.
    #[must_use]
    pub fn reruns_applied(&self, run: u64) -> usize {
        self.state()
            .reruns_applied
            .get(&run)
            .copied()
            .unwrap_or_default()
    }

    /// How many GraphQL requests sent the mutation document named `name` (`RcGitMergePullRequest`,
    /// say), scripted or not: each kind of mutation counted apart.
    #[must_use]
    pub fn mutations_named(&self, name: &str) -> usize {
        let prefix = format!(r#"{{"query":"mutation {name}("#);
        self.state()
            .requests
            .iter()
            .filter(|request| {
                request.target == "/api/graphql" && request.body.starts_with(prefix.as_bytes())
            })
            .count()
    }

    /// Whether the scripted pull request merged.
    #[must_use]
    pub fn merged(&self) -> bool {
        self.state()
            .merge
            .as_ref()
            .is_some_and(|merge| merge.merged)
    }

    /// How many requests that could change something arrived so far, counted apart from the
    /// reads: every GraphQL request whose body names a `mutation`, scripted or not, and every
    /// REST API request but a `GET`.
    #[must_use]
    pub fn mutation_requests(&self) -> usize {
        self.state()
            .requests
            .iter()
            .filter(|request| is_mutation(request))
            .count()
    }

    /// How many requests that change nothing arrived at the GraphQL endpoint so far.
    #[must_use]
    pub fn graphql_reads(&self) -> usize {
        self.state()
            .requests
            .iter()
            .filter(|request| request.target == "/api/graphql" && !is_mutation(request))
            .count()
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
            stall(stop);
            return;
        }
        ("POST", "/api/graphql") => match graphql(&request, state) {
            Served::Answer(answer) => answer,
            Served::Stall => {
                stall(stop);
                return;
            }
            Served::Drop => return,
        },
        ("GET", target) if target.ends_with(".git/info/refs") => git_challenge(),
        ("GET", target) if is_actions_read(target) => actions_read(&request, target, state),
        (method, target) if method == "GET" && target.starts_with("/api/v3/repos/") => {
            repository(&request, target, state)
        }
        ("POST", target)
            if target.starts_with("/api/v3/repos/") && target.ends_with("/rerun-failed-jobs") =>
        {
            match rerun(&request, target, state) {
                Served::Answer(answer) => answer,
                Served::Stall => {
                    stall(stop);
                    return;
                }
                Served::Drop => return,
            }
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

/// The answer to a git fetch's ref advertisement: a Basic challenge, whatever was sent.
fn git_challenge() -> Answer {
    let mut answer = json(
        "401 Unauthorized",
        r#"{"message":"Bad credentials"}"#.to_owned(),
    );
    answer.headers.push((
        "WWW-Authenticate".to_owned(),
        "Basic realm=\"GitHub\"".to_owned(),
    ));
    answer
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

/// Holds a connection open without answering until the server stops.
fn stall(stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Whether `request` could change something: a GraphQL request naming a `mutation` anywhere in
/// its body, or a REST API request other than a `GET`.
fn is_mutation(request: &RecordedRequest) -> bool {
    let target = request.target.split('?').next().unwrap_or_default();
    if target == "/api/graphql" {
        return String::from_utf8_lossy(&request.body).contains("mutation");
    }
    target.starts_with("/api/v3/") && request.method != "GET"
}

/// What the GraphQL endpoint does with one request.
enum Served {
    Answer(Answer),
    /// Hold the connection without answering.
    Stall,
    /// Close the connection without answering.
    Drop,
}

fn graphql(request: &RecordedRequest, state: &Mutex<State>) -> Served {
    let mut state = lock(state);
    if authorized(request, &state, "application/vnd.github+json").is_none() {
        return Served::Answer(unauthorized());
    }
    if request.header("content-type") != Some("application/json") {
        return Served::Answer(bad_request("GraphQL takes a JSON body"));
    }
    let Ok(body) = String::from_utf8(request.body.clone()) else {
        return Served::Answer(bad_request("the body is not UTF-8"));
    };
    if let Some(merge) = state.merge.as_mut()
        && let Some(served) = merge_request(merge, &body)
    {
        return served;
    }
    if body.starts_with(r#"{"query":"mutation"#) {
        return Served::Answer(bad_request(
            "the fake takes no mutation but the one scripted",
        ));
    }
    if let Some((_, scripted)) = state.graphql.iter().find(|(expected, _)| *expected == body) {
        let mut answer = json("200 OK", scripted.body.clone());
        answer.headers.clone_from(&scripted.headers);
        return Served::Answer(answer);
    }
    if state.graphql_echo {
        return Served::Answer(json("200 OK", format!(r#"{{"data":{{"echo":{body}}}}}"#)));
    }
    Served::Answer(bad_request(
        "the fake has no answer for this GraphQL request",
    ))
}

/// The scripted pull request's answer to `body`: its read, or its merge mutation; `None` for
/// anything else.
fn merge_request(merge: &mut MergeState, body: &str) -> Option<Served> {
    if body == merge.script.read {
        let answer = if merge.merged {
            &merge.script.merged
        } else if merge.moved() {
            &merge.script.moved
        } else {
            &merge.script.open
        };
        let answer = json("200 OK", answer.clone());
        merge.reads += 1;
        return Some(Served::Answer(answer));
    }
    if body != merge.script.mutation {
        return None;
    }
    if merge.merged {
        return Some(Served::Answer(json(
            "200 OK",
            r#"{"data":{"mergePullRequest":null},"errors":[{"type":"UNPROCESSABLE","message":"Pull Request is not mergeable"}]}"#.to_owned(),
        )));
    }
    if merge.moved()
        && let MergeBehaviour::MoveHeadAfter { refusal, .. } = &merge.script.behaviour
    {
        return Some(Served::Answer(json("200 OK", refusal.clone())));
    }
    Some(match &merge.script.behaviour {
        MergeBehaviour::Merge
        | MergeBehaviour::MoveHeadAfter { .. }
        | MergeBehaviour::ChangeAfter { .. } => {
            merge.merged = true;
            Served::Answer(json("200 OK", merge.script.reply.clone()))
        }
        MergeBehaviour::Refuse { status, body } => Served::Answer(json(status, body.clone())),
        MergeBehaviour::Answer {
            merges,
            body,
            headers,
        } => {
            merge.merged = *merges;
            let mut answer = json("200 OK", body.clone());
            answer.headers.clone_from(headers);
            Served::Answer(answer)
        }
        MergeBehaviour::MergeThenStall => {
            merge.merged = true;
            Served::Stall
        }
        MergeBehaviour::MergeThenDrop => {
            merge.merged = true;
            Served::Drop
        }
        MergeBehaviour::Drop => Served::Drop,
    })
}

/// `owner/name` and the run id of a re-run path `/api/v3/repos/<owner>/<name>/actions/runs/<id>/
/// rerun-failed-jobs`, when it is exactly that shape with a decimal id.
fn rerun_path(target: &str) -> Option<(String, u64)> {
    let rest = target
        .strip_prefix("/api/v3/repos/")?
        .strip_suffix("/rerun-failed-jobs")?;
    let parts: Vec<&str> = rest.split('/').collect();
    let [owner, name, "actions", "runs", id] = parts.as_slice() else {
        return None;
    };
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((format!("{owner}/{name}"), id.parse().ok()?))
}

/// The answer to a re-run of failed jobs: only for a correct request (the API headers, the
/// token, a JSON body that is exactly [`RERUN_BODY`]) to a scripted run.
fn rerun(request: &RecordedRequest, target: &str, state: &Mutex<State>) -> Served {
    let mut state = lock(state);
    if authorized(request, &state, "application/vnd.github+json").is_none() {
        return Served::Answer(unauthorized());
    }
    if request.header("content-type") != Some("application/json")
        || request.body != RERUN_BODY.as_bytes()
    {
        return Served::Answer(bad_request(
            "a re-run sends exactly {\"enable_debug_logging\":false}",
        ));
    }
    let Some((repository, run)) = rerun_path(target) else {
        return Served::Answer(not_found());
    };
    let Some(behaviour) = state.reruns.get(&(repository, run)).cloned() else {
        return Served::Answer(not_found());
    };
    let mut apply = || *state.reruns_applied.entry(run).or_default() += 1;
    match behaviour {
        RerunBehaviour::Rerun => {
            apply();
            Served::Answer(json("201 Created", "{}".to_owned()))
        }
        RerunBehaviour::Answer {
            status,
            body,
            headers,
        } => {
            let mut answer = json(status, body);
            answer.headers = headers;
            Served::Answer(answer)
        }
        RerunBehaviour::RerunThenDrop => {
            apply();
            Served::Drop
        }
        RerunBehaviour::Drop => Served::Drop,
    }
}

/// Whether `target` is a job's read, a job's logs or a workflow run's read:
/// `/api/v3/repos/<owner>/<name>/actions/jobs/<id>`, `.../jobs/<id>/logs` or `.../runs/<id>`.
fn is_actions_read(target: &str) -> bool {
    let Some(rest) = target.strip_prefix("/api/v3/repos/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [_, _, "actions", "jobs" | "runs", id] | [_, _, "actions", "jobs", id, "logs"] => {
            !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

/// The answer to a job's read, its logs or a workflow run's read: only with the API headers and a
/// known token, and only as scripted for exactly that path; anything else is a `404`.
fn actions_read(request: &RecordedRequest, target: &str, state: &Mutex<State>) -> Answer {
    let state = lock(state);
    if authorized(request, &state, "application/vnd.github+json").is_none() {
        return unauthorized();
    }
    let path = target.trim_start_matches("/api/v3");
    if let Some(answer) = state.logs.get(path) {
        return match answer {
            LogsAnswer::Redirect { status, location } => Answer {
                status,
                headers: vec![("Location".to_owned(), location.clone())],
                body: Vec::new(),
                streamed: false,
            },
            LogsAnswer::Answer { status, body } => json(status, body.clone()),
        };
    }
    match state.actions.get(path) {
        Some((status, body)) => json(status, body.clone()),
        None => not_found(),
    }
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

/// What the [`StorageServer`] answers every request with.
#[derive(Debug, Clone)]
pub enum StorageBehaviour {
    /// The log `body`. With `ranges`, a `Range: bytes=-<n>` request gets `206 Partial Content`
    /// with its last `n` bytes and a `Content-Range`; otherwise, and for any other request, `200
    /// OK` with the whole body.
    Log {
        /// The log.
        body: Vec<u8>,
        /// Whether a suffix range is honoured.
        ranges: bool,
    },
    /// `200 OK` with `bytes` bytes of log lines and no `Content-Length`, ending at close.
    Streamed {
        /// How many bytes.
        bytes: usize,
    },
    /// Any status, headers and body, exactly as given.
    Answer {
        /// The status line.
        status: &'static str,
        /// Extra response headers, as `(name, value)`.
        headers: Vec<(String, String)>,
        /// The body.
        body: Vec<u8>,
    },
    /// Holds the connection open without answering until the server stops.
    Stall,
}

#[derive(Debug)]
struct StorageState {
    behaviour: StorageBehaviour,
    requests: Vec<RecordedRequest>,
}

/// A second loopback host standing in for where GitHub keeps a job's log: it answers every
/// request as its [`StorageBehaviour`] says and records each one, headers included, so a test
/// sees exactly what the download sent.
#[derive(Debug)]
pub struct StorageServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<StorageState>>,
    handle: Option<JoinHandle<()>>,
}

impl StorageServer {
    /// A server answering `behaviour`.
    ///
    /// # Panics
    /// When the listener cannot bind.
    #[must_use]
    pub fn start(behaviour: StorageBehaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("local addr").port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(StorageState {
            behaviour,
            requests: Vec::new(),
        }));
        let (stop, shared) = (Arc::clone(&shutdown), Arc::clone(&state));
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
                std::thread::spawn(move || serve_storage(stream, &shared, &stop));
            }
        });
        Self {
            port,
            shutdown,
            state,
            handle: Some(handle),
        }
    }

    /// `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// From now on answers as `behaviour` says.
    pub fn set(&self, behaviour: StorageBehaviour) {
        self.storage().behaviour = behaviour;
    }

    /// Every request so far, in arrival order.
    #[must_use]
    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.storage().requests.clone()
    }

    /// How many requests arrived so far.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.storage().requests.len()
    }

    fn storage(&self) -> MutexGuard<'_, StorageState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for StorageServer {
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

fn serve_storage(stream: TcpStream, state: &Mutex<StorageState>, stop: &AtomicBool) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let Some(request) = read_request(&mut reader) else {
        return;
    };
    let behaviour = {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.requests.push(request.clone());
        state.behaviour.clone()
    };
    let answer = match behaviour {
        StorageBehaviour::Log { body, ranges } => log_answer(&request, body, ranges),
        StorageBehaviour::Streamed { bytes } => Answer {
            status: "200 OK",
            headers: Vec::new(),
            body: b"line\n".iter().copied().cycle().take(bytes).collect(),
            streamed: true,
        },
        StorageBehaviour::Answer {
            status,
            headers,
            body,
        } => Answer {
            status,
            headers,
            body,
            streamed: false,
        },
        StorageBehaviour::Stall => {
            stall(stop);
            return;
        }
    };
    let mut writer = stream;
    write_answer(&mut writer, &answer);
}

/// The log `body` for `request`: its last `n` bytes as a `206` when `ranges` and the request asks
/// for `bytes=-<n>`, else the whole of it as a `200`.
fn log_answer(request: &RecordedRequest, body: Vec<u8>, ranges: bool) -> Answer {
    let suffix = request
        .header("range")
        .and_then(|range| range.strip_prefix("bytes=-"))
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|_| ranges);
    let Some(n) = suffix.filter(|n| *n > 0 && !body.is_empty()) else {
        return Answer {
            status: "200 OK",
            headers: Vec::new(),
            body,
            streamed: false,
        };
    };
    let total = body.len();
    let first = total.saturating_sub(n);
    Answer {
        status: "206 Partial Content",
        headers: vec![(
            "Content-Range".to_owned(),
            format!("bytes {first}-{}/{total}", total - 1),
        )],
        body: body[first..].to_vec(),
        streamed: false,
    }
}
