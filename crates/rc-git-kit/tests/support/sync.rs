//! The doubles `SyncModelTests.swift` built: a scripted `RepositoryActions` whose steps
//! answer queued results in order and can be held mid-call, a credential store that records
//! every host it was asked for, and shared host-key approvals.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::{DefaultBranch, Oid, StashEntry};
use rc_git_guard::actions::{
    ActionError, ActionResult, CheckoutOutcome, CommitRequest, Committed, Deleted, DiscardOutcome,
    FetchOutcome, HunkStamp, Identity, PullOutcome, PushOutcome, RefUpdate, StashRequest,
    StashSaved, UpstreamStatus,
};
use rc_git_kit::fake::{CallGate, oid};
use rc_git_kit::guard_bridge::{CancelToken, Credentials, GuardDecision};
use rc_git_kit::oneshot::{self, Receiver};
use rc_git_kit::platform::credentials::{CredentialError, CredentialKind, CredentialStore};
use rc_git_kit::platform::host_keys::{HostKeyApproval, HostKeyStore, MemoryHostKeyStore};
use rc_git_kit::platform::settings::SettingsError;
use rc_git_kit::repository_actions::{RemoteRequest, RepositoryActions};
use rc_git_kit::sync::{
    CredentialReply, CredentialRequest, HostKeyReply, HostKeyRequest, SyncEvent, SyncModel,
    SyncState,
};

/// A scripted failure whose message is exactly `message`, as Swift's `.failure(message:)`.
pub fn failure(message: &str) -> ActionError {
    ActionError::Denied {
        reason: message.to_owned(),
    }
}

pub fn auth_required(host: &str) -> ActionError {
    ActionError::AuthRequired {
        op: "fetch",
        host: host.to_owned(),
    }
}

pub fn passphrase_required(key: &str) -> ActionError {
    ActionError::PassphraseRequired {
        op: "fetch",
        host: "git.example.com".to_owned(),
        key: key.to_owned(),
    }
}

pub fn unknown_host_key(fingerprint: &str) -> ActionError {
    ActionError::UnknownHostKey {
        op: "fetch",
        host: "git.example.com".to_owned(),
        key_type: "ssh-ed25519".to_owned(),
        fingerprint: fingerprint.to_owned(),
    }
}

pub fn upstream(host: &str, ahead: usize, behind: usize) -> UpstreamStatus {
    UpstreamStatus {
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        upstream_ref: "refs/remotes/origin/main".to_owned(),
        url: format!("https://{host}/repo.git"),
        host: host.to_owned(),
        ahead,
        behind,
        has_upstream: true,
        published: true,
        uses_ssh: false,
    }
}

pub fn ssh_upstream(host: &str) -> UpstreamStatus {
    UpstreamStatus {
        url: format!("{host}:repo.git"),
        uses_ssh: true,
        ..upstream(host, 0, 0)
    }
}

pub fn no_upstream(branch: &str) -> UpstreamStatus {
    UpstreamStatus {
        branch: branch.to_owned(),
        remote: String::new(),
        upstream_ref: String::new(),
        url: String::new(),
        host: String::new(),
        ahead: 0,
        behind: 0,
        has_upstream: false,
        published: false,
        uses_ssh: false,
    }
}

pub fn fetched(refs: usize) -> FetchOutcome {
    FetchOutcome {
        remote: "origin".to_owned(),
        updated_refs: (0..refs)
            .map(|_| RefUpdate {
                name: "refs/remotes/origin/main".to_owned(),
                target: [0u8; 20].into(),
            })
            .collect(),
    }
}

pub fn pulled(files: usize) -> PullOutcome {
    PullOutcome {
        branch: "main".to_owned(),
        old: oid(1),
        new: oid(2),
        files_changed: files,
        already_up_to_date: files == 0,
        unpublished: false,
    }
}

pub fn pushed(commits: usize) -> PushOutcome {
    PushOutcome {
        remote: "origin".to_owned(),
        branch: "main".to_owned(),
        old: Some(oid(1)),
        new: oid(2),
        pushed_commits: commits,
        already_up_to_date: commits == 0,
        tracking_ref_error: None,
    }
}

pub fn switched(already_on_branch: bool, reflog_written: bool) -> CheckoutOutcome {
    CheckoutOutcome {
        branch: "main".to_owned(),
        new_head: oid(0),
        files_changed: if already_on_branch { 0 } else { 4 },
        already_on_branch,
        reflog_written,
    }
}

/// What each step answers, in call order.
pub struct Script {
    /// One answer per status read, the last one repeating; `None` makes that read fail.
    pub preflights: Vec<Option<UpstreamStatus>>,
    pub default_branch: Option<DefaultBranch>,
    pub default_branch_fails: bool,
    pub fetches: VecDeque<ActionResult<FetchOutcome>>,
    pub pulls: VecDeque<ActionResult<PullOutcome>>,
    pub checkouts: VecDeque<ActionResult<CheckoutOutcome>>,
    pub pushes: VecDeque<ActionResult<PushOutcome>>,
}

impl Script {
    pub fn new(preflights: Vec<Option<UpstreamStatus>>) -> Self {
        Self {
            preflights,
            default_branch: Some(DefaultBranch {
                name: "main".to_owned(),
                exact_name: true,
            }),
            default_branch_fails: false,
            fetches: VecDeque::new(),
            pulls: VecDeque::new(),
            checkouts: VecDeque::new(),
            pushes: VecDeque::new(),
        }
    }

    pub fn upstream() -> Self {
        Self::new(vec![Some(upstream("example.com", 0, 0))])
    }

    pub fn fetches(mut self, answers: Vec<ActionResult<FetchOutcome>>) -> Self {
        self.fetches = answers.into();
        self
    }

    pub fn pulls(mut self, answers: Vec<ActionResult<PullOutcome>>) -> Self {
        self.pulls = answers.into();
        self
    }

    pub fn pushes(mut self, answers: Vec<ActionResult<PushOutcome>>) -> Self {
        self.pushes = answers.into();
        self
    }

    pub fn checkouts(mut self, answers: Vec<ActionResult<CheckoutOutcome>>) -> Self {
        self.checkouts = answers.into();
        self
    }
}

/// A step a test can hold mid-call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Preflight,
    Fetch,
    Pull,
    Checkout,
    Push,
}

/// A held call: resolves `arrived` once it is inside the step, then waits at `gate`.
pub struct Held {
    pub arrived: Receiver<()>,
    pub gate: Arc<CallGate>,
}

struct Hold {
    arrived: oneshot::Sender<()>,
    gate: Arc<CallGate>,
}

pub struct StubState {
    script: Script,
    pub fetch_requests: Vec<RemoteRequest>,
    pub push_requests: Vec<RemoteRequest>,
    pub checkout_branches: Vec<String>,
    pub pull_calls: usize,
    pub push_calls: usize,
    pub preflight_calls: usize,
    pub default_branch_calls: usize,
    /// Per network call, whether its token was signalled when it answered.
    pub tokens_signalled: Vec<bool>,
    /// Signals the network step's own token inside the step, as a transfer that saw the
    /// flag would; stands in for Swift's `duringStep = { $0.signal() }`.
    pub signal_in_step: bool,
    holds: HashMap<Step, Hold>,
}

impl StubState {
    /// Queues one more fetch answer.
    pub fn queue_fetch(&mut self, answer: ActionResult<FetchOutcome>) {
        self.script.fetches.push_back(answer);
    }
}

/// The scripted actions. Cloning shares the state.
#[derive(Clone)]
pub struct Stub {
    state: Arc<Mutex<StubState>>,
}

fn missing(step: &str) -> ActionError {
    failure(&format!("unexpected {step}"))
}

impl Stub {
    pub fn new(script: Script) -> Self {
        Self {
            state: Arc::new(Mutex::new(StubState {
                script,
                fetch_requests: Vec::new(),
                push_requests: Vec::new(),
                checkout_branches: Vec::new(),
                pull_calls: 0,
                push_calls: 0,
                preflight_calls: 0,
                default_branch_calls: 0,
                tokens_signalled: Vec::new(),
                signal_in_step: false,
                holds: HashMap::new(),
            })),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, StubState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Holds the next call of `step`.
    pub fn hold(&self, step: Step) -> Held {
        let (arrived, receiver) = oneshot::channel();
        let gate = Arc::new(CallGate::new());
        let hold = Hold {
            arrived,
            gate: Arc::clone(&gate),
        };
        self.state().holds.insert(step, hold);
        Held {
            arrived: receiver,
            gate,
        }
    }

    fn pass(&self, step: Step) {
        let hold = self.state().holds.remove(&step);
        if let Some(hold) = hold {
            hold.arrived.send(());
            hold.gate.pass();
        }
    }

    fn network(&self, cancel: &CancelToken) {
        if self.state().signal_in_step {
            cancel.signal();
        }
    }

    pub fn fetch_tokens(&self) -> Vec<Option<String>> {
        let state = self.state();
        state
            .fetch_requests
            .iter()
            .map(|request| request.credentials.as_ref().map(|c| c.token.clone()))
            .collect()
    }

    pub fn push_tokens(&self) -> Vec<Option<String>> {
        let state = self.state();
        state
            .push_requests
            .iter()
            .map(|request| request.credentials.as_ref().map(|c| c.token.clone()))
            .collect()
    }

    pub fn fetch_host_keys(&self) -> Vec<Option<String>> {
        let state = self.state();
        state
            .fetch_requests
            .iter()
            .map(|request| request.accepted_host_key.clone())
            .collect()
    }
}

impl RepositoryActions for Stub {
    fn plan_branch_delete(&self, _name: &str) -> ActionResult<GuardDecision> {
        Err(missing("plan_branch_delete"))
    }

    fn create_branch(&self, _name: &str, _revspec: &str) -> ActionResult<()> {
        Err(missing("create_branch"))
    }

    fn create_tag(&self, _name: &str, _revspec: &str) -> ActionResult<()> {
        Err(missing("create_tag"))
    }

    fn delete_branch(&self, _name: &str, _input: Option<&str>) -> ActionResult<Deleted> {
        Err(missing("delete_branch"))
    }

    fn delete_tag(
        &self,
        _name: &str,
        _expected: Oid,
        _input: Option<&str>,
    ) -> ActionResult<Deleted> {
        Err(missing("delete_tag"))
    }

    fn configured_identity(&self) -> ActionResult<Option<Identity>> {
        Err(missing("configured_identity"))
    }

    fn commit(&self, _request: CommitRequest<'_>, _input: Option<&str>) -> ActionResult<Committed> {
        Err(missing("commit"))
    }

    fn stash_push(
        &self,
        _request: StashRequest<'_>,
        _input: Option<&str>,
    ) -> ActionResult<StashSaved> {
        Err(missing("stash_push"))
    }

    fn stash_apply(&self, _entry: &StashEntry, _input: Option<&str>) -> ActionResult<()> {
        Err(missing("stash_apply"))
    }

    fn stash_pop(&self, _entry: &StashEntry, _input: Option<&str>) -> ActionResult<Deleted> {
        Err(missing("stash_pop"))
    }

    fn stash_drop(&self, _entry: &StashEntry, _input: Option<&str>) -> ActionResult<Deleted> {
        Err(missing("stash_drop"))
    }

    fn discard_files(
        &self,
        _paths: &[String],
        _input: Option<&str>,
    ) -> ActionResult<Vec<DiscardOutcome>> {
        Err(missing("discard_files"))
    }

    fn discard_hunks(
        &self,
        _path: &str,
        _stamps: &[HunkStamp],
        _input: Option<&str>,
    ) -> ActionResult<()> {
        Err(missing("discard_hunks"))
    }

    fn upstream_status(&self) -> ActionResult<UpstreamStatus> {
        let next = {
            let mut state = self.state();
            state.preflight_calls += 1;
            let preflights = &mut state.script.preflights;
            if preflights.len() > 1 {
                Some(preflights.remove(0))
            } else {
                preflights.first().cloned()
            }
        };
        self.pass(Step::Preflight);
        match next {
            None => Err(failure("no preflight scripted")),
            Some(None) => Err(failure("preflight failed")),
            Some(Some(status)) => Ok(status),
        }
    }

    fn default_branch(&self) -> ActionResult<Option<DefaultBranch>> {
        let mut state = self.state();
        state.default_branch_calls += 1;
        if state.script.default_branch_fails {
            return Err(failure("cannot open /tmp/repo"));
        }
        Ok(state.script.default_branch.clone())
    }

    fn fetch(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<FetchOutcome> {
        self.network(cancel);
        self.pass(Step::Fetch);
        let mut state = self.state();
        state.fetch_requests.push(remote.clone());
        state.tokens_signalled.push(cancel.is_signalled());
        state
            .script
            .fetches
            .pop_front()
            .unwrap_or_else(|| Err(missing("fetch")))
    }

    fn pull_fast_forward(&self) -> ActionResult<PullOutcome> {
        self.pass(Step::Pull);
        let mut state = self.state();
        state.pull_calls += 1;
        state
            .script
            .pulls
            .pop_front()
            .unwrap_or_else(|| Err(missing("pull")))
    }

    fn checkout(&self, branch: &str) -> ActionResult<CheckoutOutcome> {
        self.pass(Step::Checkout);
        let mut state = self.state();
        state.checkout_branches.push(branch.to_owned());
        state
            .script
            .checkouts
            .pop_front()
            .unwrap_or_else(|| Err(missing("checkout")))
    }

    fn push(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<PushOutcome> {
        self.network(cancel);
        self.pass(Step::Push);
        let mut state = self.state();
        state.push_requests.push(remote.clone());
        state.push_calls += 1;
        state.tokens_signalled.push(cancel.is_signalled());
        state
            .script
            .pushes
            .pop_front()
            .unwrap_or_else(|| Err(missing("push")))
    }
}

#[derive(Default)]
struct StoreState {
    items: HashMap<(String, CredentialKind), Credentials>,
    queried: Vec<String>,
    writes: usize,
}

/// A credential store that records every host it was asked for.
#[derive(Clone, Default)]
pub struct TestStore {
    state: Arc<Mutex<StoreState>>,
    fails_reads: bool,
    fails_writes: bool,
}

impl TestStore {
    pub fn with(items: &[(&str, CredentialKind, Credentials)]) -> Self {
        let store = Self::default();
        for (host, kind, credentials) in items {
            store
                .lock()
                .items
                .insert(((*host).to_owned(), *kind), credentials.clone());
        }
        store
    }

    pub fn failing_reads() -> Self {
        Self {
            fails_reads: true,
            ..Self::default()
        }
    }

    pub fn failing_writes() -> Self {
        Self {
            fails_writes: true,
            ..Self::default()
        }
    }

    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn queried_hosts(&self) -> Vec<String> {
        self.lock().queried.clone()
    }

    pub fn write_count(&self) -> usize {
        self.lock().writes
    }

    pub fn stored(&self, host: &str, kind: CredentialKind) -> Option<String> {
        self.lock()
            .items
            .get(&(host.to_owned(), kind))
            .map(|credentials| credentials.token.clone())
    }

    pub fn len(&self) -> usize {
        self.lock().items.len()
    }
}

impl CredentialStore for TestStore {
    fn credentials(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<Option<Credentials>, CredentialError> {
        let mut state = self.lock();
        state.queried.push(host.to_owned());
        if self.fails_reads {
            return Err(CredentialError {
                op: "read",
                host: host.to_owned(),
                status: -25293,
            });
        }
        Ok(state.items.get(&(host.to_owned(), kind)).cloned())
    }

    fn store(
        &self,
        credentials: &Credentials,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(), CredentialError> {
        let mut state = self.lock();
        state.writes += 1;
        if self.fails_writes {
            return Err(CredentialError {
                op: "write",
                host: host.to_owned(),
                status: -25293,
            });
        }
        state
            .items
            .insert((host.to_owned(), kind), credentials.clone());
        Ok(())
    }
}

/// Approvals shared between the model and a Settings list in the same test.
#[derive(Clone, Default)]
pub struct SharedHostKeys(pub Arc<MemoryHostKeyStore>);

impl SharedHostKeys {
    pub fn with(entries: &[(&str, &str)]) -> Self {
        let keys = Self::default();
        for (host, fingerprint) in entries {
            keys.0.store(fingerprint, host).unwrap_or_default();
        }
        keys
    }

    pub fn stored(&self) -> Vec<(String, String)> {
        self.0
            .approvals()
            .into_iter()
            .map(|approval| (approval.host, approval.fingerprint))
            .collect()
    }
}

impl HostKeyStore for SharedHostKeys {
    fn fingerprint(&self, host: &str) -> Option<String> {
        self.0.fingerprint(host)
    }

    fn store(&self, fingerprint: &str, host: &str) -> Result<(), SettingsError> {
        self.0.store(fingerprint, host)
    }

    fn forget(&self, host: &str) -> Result<(), SettingsError> {
        self.0.forget(host)
    }

    fn approvals(&self) -> Vec<HostKeyApproval> {
        self.0.approvals()
    }
}

pub fn credentials(username: &str, token: &str) -> Credentials {
    Credentials {
        username: username.to_owned(),
        token: token.to_owned(),
    }
}

/// A model over `stub`, `store` and `host_keys`, with parking allowed for its threads.
pub fn model(
    cx: &mut TestAppContext,
    stub: &Stub,
    store: &TestStore,
    host_keys: &SharedHostKeys,
) -> Entity<SyncModel> {
    cx.executor().allow_parking();
    let actions = Arc::new(stub.clone());
    let store = Arc::new(store.clone());
    let host_keys = Arc::new(host_keys.clone());
    cx.new(|_| SyncModel::new(actions, store, host_keys))
}

/// Every workflow state the model announced, in order.
pub fn states(cx: &mut TestAppContext, model: &Entity<SyncModel>) -> Rc<RefCell<Vec<SyncState>>> {
    let log = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&log);
    cx.update(|cx| {
        cx.subscribe(model, move |_, event: &SyncEvent, _| {
            if let SyncEvent::StateChanged(state) = event {
                sink.borrow_mut().push(state.clone());
            }
        })
        .detach();
    });
    log
}

/// Answers every credentials prompt with `reply`, counting the prompts and keeping the last
/// request.
pub fn answer_credentials(
    cx: &mut TestAppContext,
    model: &Entity<SyncModel>,
    reply: CredentialReply,
) -> Rc<RefCell<Vec<CredentialRequest>>> {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    model.update(cx, |m, _| {
        m.set_credential_prompter(Rc::new(move |request, answer, _| {
            sink.borrow_mut().push(request);
            answer.send(reply.clone());
        }));
    });
    asked
}

/// Answers every host-key prompt with `reply`, keeping each request.
pub fn answer_host_keys(
    cx: &mut TestAppContext,
    model: &Entity<SyncModel>,
    reply: HostKeyReply,
) -> Rc<RefCell<Vec<HostKeyRequest>>> {
    let asked = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    model.update(cx, |m, _| {
        m.set_host_key_prompter(Rc::new(move |request, answer, _| {
            sink.borrow_mut().push(request);
            answer.send(reply);
        }));
    });
    asked
}

pub fn state(cx: &mut TestAppContext, model: &Entity<SyncModel>) -> SyncState {
    model.read_with(cx, |m, _| m.state().clone())
}

pub async fn refresh(cx: &mut TestAppContext, model: &Entity<SyncModel>) {
    model.update(cx, SyncModel::refresh_status).await;
}
