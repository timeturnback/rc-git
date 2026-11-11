//! Port of `SyncModel.swift`: Fetch, Pull, Push, Sync and Back to default as workflows over
//! [`RepositoryActions`] (`upstream_status`, `fetch`, `pull_fast_forward`, `push`,
//! `default_branch`, `checkout`), busy-gated and credential-negotiating, with every blocking
//! call on its own thread through [`crate::blocking::blocking`]. A guard remote action builds
//! its own tokio runtime, so it never runs inside one; those threads are not.
//!
//! The model renders nothing. A network step that needs a secret or a host-key approval asks
//! through the prompter the window set ([`SyncModel::set_credential_prompter`],
//! [`SyncModel::set_host_key_prompter`]); a window with none answers "unavailable", which
//! fails the step with a message saying nobody could be asked.

mod flow;
mod network;
mod values;

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Context, EventEmitter, Task};
use rc_git_core::DefaultBranch;
use rc_git_guard::actions::UpstreamStatus;

pub use values::{
    CredentialEntry, CredentialReply, CredentialRetry, HostKeyReply, SyncAvailability,
    SyncConditions, SyncReport, SyncState, SyncStep, SyncSummary, back_to_default_title,
    changed_host_key_message, checkout_warning, credential_retry, idle_sync_tooltip,
    no_prompt_message, no_upstream_message, pull_title, push_title, push_warning,
    reconcile_command,
};

use crate::guard_bridge::{CancelToken, Credentials};
use crate::oneshot;
use crate::platform::credentials::{CredentialKind, CredentialStore};
use crate::platform::host_keys::HostKeyStore;
use crate::repository_actions::RepositoryActions;
use flow::{Flow, Halt, Outcome, Workflow};

/// What a network step asks the user for: a secret for `host`, an HTTPS token or the
/// passphrase of the locked ssh key at `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRequest {
    /// The host the secret is for; the guard offers it to no other.
    pub host: String,
    /// What to ask for.
    pub kind: CredentialKind,
    /// The locked key's path over ssh, `None` over HTTPS.
    pub key: Option<String>,
}

/// What a network step asks the user to approve: an unknown ssh host's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyRequest {
    /// The host.
    pub host: String,
    /// `"<key type> <fingerprint>"`, for the user to verify.
    pub host_key: String,
}

/// Puts a credentials sheet on screen and answers through the sender: `Provided` retries,
/// `Cancelled` fails the step, and a sender dropped unanswered counts as `Unavailable`.
pub type CredentialPrompter =
    Rc<dyn Fn(CredentialRequest, oneshot::Sender<CredentialReply>, &mut App)>;

/// Puts a host-key trust sheet on screen and answers through the sender, as
/// [`CredentialPrompter`] does.
pub type HostKeyPrompter = Rc<dyn Fn(HostKeyRequest, oneshot::Sender<HostKeyReply>, &mut App)>;

/// What moved, so a view redraws only that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    /// The workflow state moved to this one.
    StateChanged(SyncState),
    /// A status read landed: [`SyncModel::remote_status`] and [`SyncModel::status_error`],
    /// and the mainline branch, may have changed.
    StatusChanged,
}

/// The credentials a workflow settled on for one host and kind; `None` when none are known.
#[derive(Debug, Clone)]
struct WorkflowCredentials {
    host: String,
    kind: CredentialKind,
    credentials: Option<Credentials>,
}

/// The status read's coalescing pair and generation.
#[derive(Debug, Default)]
struct StatusLoop {
    running: bool,
    pending: bool,
    generation: u64,
}

/// The sync workflows of one repository window.
pub struct SyncModel {
    actions: Arc<dyn RepositoryActions>,
    store: Arc<dyn CredentialStore>,
    host_keys: Arc<dyn HostKeyStore>,
    state: SyncState,
    /// The last status read; `None` before the first read and after a failed one, so a stale
    /// count is never shown as current.
    remote_status: Option<UpstreamStatus>,
    status_error: Option<String>,
    /// The mainline branch with its exactness: a name repaired with U+FFFD names the control
    /// but is never checked out.
    mainline: Option<DefaultBranch>,
    /// Whether `mainline` holds an answer, so a repository that names no mainline is not
    /// re-read on every refresh.
    default_branch_read: bool,
    status: StatusLoop,
    /// The branch the last workflow checked out, set the moment the switch lands and kept
    /// whatever ends the workflow, because `HEAD` and the working tree have moved either way.
    switched_branch: Option<String>,
    /// The credentials the running workflow settled on: read once per workflow, so a Sync
    /// costs one keychain read and its push never prompts for what its fetch was just given.
    workflow_credentials: Option<WorkflowCredentials>,
    /// A credential- or approval-store failure in the running workflow, reported with its
    /// outcome.
    credential_notice: Option<String>,
    generation: u64,
    /// The running workflow's flag. One per workflow: a signalled token stays signalled, so
    /// the next workflow gets a fresh one.
    cancel: Arc<CancelToken>,
    credential_prompter: Option<CredentialPrompter>,
    host_key_prompter: Option<HostKeyPrompter>,
}

impl EventEmitter<SyncEvent> for SyncModel {}

impl SyncModel {
    /// A model running `actions`, reading and saving secrets in `store` (the app hands it a
    /// [`crate::platform::credentials::CachingCredentialStore`]) and host-key approvals in
    /// `host_keys`.
    #[must_use]
    pub fn new(
        actions: Arc<dyn RepositoryActions>,
        store: Arc<dyn CredentialStore>,
        host_keys: Arc<dyn HostKeyStore>,
    ) -> Self {
        Self {
            actions,
            store,
            host_keys,
            state: SyncState::Idle,
            remote_status: None,
            status_error: None,
            mainline: None,
            default_branch_read: false,
            status: StatusLoop::default(),
            switched_branch: None,
            workflow_credentials: None,
            credential_notice: None,
            generation: 0,
            cancel: Arc::new(CancelToken::new()),
            credential_prompter: None,
            host_key_prompter: None,
        }
    }

    /// Where the workflow stands.
    #[must_use]
    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// The last status read; feeds the titles and the tooltip.
    #[must_use]
    pub fn remote_status(&self) -> Option<&UpstreamStatus> {
        self.remote_status.as_ref()
    }

    /// Why the last status read failed, `None` after a successful one.
    #[must_use]
    pub fn status_error(&self) -> Option<&str> {
        self.status_error.as_deref()
    }

    /// The repository's mainline branch, read with the first status refresh. Names the
    /// Back-to-default control and is what it switches to.
    #[must_use]
    pub fn default_branch(&self) -> Option<&str> {
        self.mainline.as_ref().map(|branch| branch.name.as_str())
    }

    /// Whether a workflow is running or cancelling.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.state.is_busy()
    }

    /// Whether the running workflow can still be asked to stop.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        self.state.is_cancellable()
    }

    /// The controls that can run now in a window whose repository has a working tree or not.
    #[must_use]
    pub fn availability(&self, has_working_tree: bool) -> SyncAvailability {
        SyncAvailability::new(SyncConditions {
            status: self.remote_status.as_ref(),
            busy: self.is_busy(),
            cancellable: self.is_cancellable(),
            has_repository: true,
            has_working_tree,
        })
    }

    /// Sets what puts the credentials sheet on screen.
    pub fn set_credential_prompter(&mut self, prompter: CredentialPrompter) {
        self.credential_prompter = Some(prompter);
    }

    /// Sets what puts the host-key trust sheet on screen.
    pub fn set_host_key_prompter(&mut self, prompter: HostKeyPrompter) {
        self.host_key_prompter = Some(prompter);
    }

    /// The branch a workflow switched to, once: the caller follows it, and a second call
    /// answers `None`. `None` while a workflow runs and when the last one switched nothing.
    pub fn take_switched_branch(&mut self) -> Option<String> {
        if self.is_busy() {
            return None;
        }
        self.switched_branch.take()
    }

    /// Returns a terminal state to idle so the next workflow starts clean.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.set_state(SyncState::Idle, cx);
    }

    /// Asks the running workflow to stop. The flag reaches the step itself, so this does not
    /// wait for a dead connection to time out. A cancel that lands during a checkout or a
    /// pull lets that write finish and stops the workflow at the next step, which still
    /// reports what the write did. A no-op when nothing is running or a cancel is already
    /// under way.
    pub fn cancel_running(&mut self, cx: &mut Context<Self>) {
        let SyncState::Running(step) = self.state else {
            return;
        };
        self.cancel.signal();
        self.set_state(SyncState::Cancelling(step), cx);
    }

    /// Fetches from the upstream.
    pub fn fetch(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.start(Workflow::Fetch, cx)
    }

    /// Fetches, then fast-forwards: what the Pull command means.
    pub fn pull(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.start(Workflow::Pull, cx)
    }

    /// Pushes to the upstream.
    pub fn push(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.start(Workflow::Push, cx)
    }

    /// Fetches, fast-forwards, then pushes; stops at the first failure or divergence.
    pub fn sync(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.start(Workflow::Sync, cx)
    }

    /// Switches to the mainline branch, then fetches and fast-forwards it. The upstream is
    /// checked after the switch, because the branch switched to is the one whose upstream
    /// matters.
    pub fn back_to_default_branch(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.start(Workflow::BackToDefault, cx)
    }

    fn set_state(&mut self, state: SyncState, cx: &mut Context<Self>) {
        self.state = state.clone();
        cx.emit(SyncEvent::StateChanged(state));
        cx.notify();
    }

    /// Moves the state to `step`. Once a cancel is signalled the workflow only unwinds, so
    /// the step shows as cancelling and never offers Stop again.
    fn enter(&mut self, step: SyncStep, cx: &mut Context<Self>) {
        let state = if self.cancel.is_signalled() {
            SyncState::Cancelling(step)
        } else {
            SyncState::Running(step)
        };
        self.set_state(state, cx);
    }

    /// Runs one workflow; a no-op while another runs.
    fn start(&mut self, workflow: Workflow, cx: &mut Context<Self>) -> Task<()> {
        if self.is_busy() {
            return Task::ready(());
        }
        self.generation += 1;
        self.cancel = Arc::new(CancelToken::new());
        self.switched_branch = None;
        self.workflow_credentials = None;
        self.credential_notice = None;
        self.enter(workflow.start(), cx);
        let flow = Flow::new(self, cx);
        cx.spawn(async move |this, cx| {
            let generation = flow.generation;
            let outcome = match flow.run(workflow, cx).await {
                Ok(summary) => Ok(summary),
                Err(Halt::Failed(failure)) => Err(failure),
                Err(Halt::Released) => return,
            };
            let refresh = this.update(cx, |model, cx| model.finish(generation, outcome, cx));
            if let Ok(Some(refresh)) = refresh {
                refresh.await;
            }
        })
    }

    /// Ends the workflow `generation` names in its terminal state, then re-reads the status.
    fn finish(
        &mut self,
        generation: u64,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        if generation != self.generation {
            return None;
        }
        self.workflow_credentials = None;
        let state = match outcome {
            Ok(summary) => {
                SyncState::Done(summary.adding_notice(self.credential_notice.as_deref()))
            }
            // What the user asked for decides the state, not which error the unwinding step
            // reported: usually the guard's own cancel, or a transport error it hit as the
            // flag was set.
            Err(_) if self.cancel.is_signalled() => SyncState::Idle,
            Err(failure) if failure.diverged => SyncState::Diverged(failure.message),
            Err(failure) => SyncState::Failed {
                step: failure.step,
                message: self.with_notice(failure.message),
            },
        };
        self.set_state(state, cx);
        Some(self.refresh_status(cx))
    }

    /// A failure message with the workflow's credential-store failure, when there was one.
    fn with_notice(&self, message: String) -> String {
        match &self.credential_notice {
            Some(notice) => format!("{message} ({notice})"),
            None => message,
        }
    }

    /// Records one status read: a failure clears the last status, so nothing reads an older
    /// count as current.
    fn apply_status(&mut self, status: Result<UpstreamStatus, String>, cx: &mut Context<Self>) {
        match status {
            Ok(status) => {
                self.remote_status = Some(status);
                self.status_error = None;
            }
            Err(message) => {
                self.remote_status = None;
                self.status_error = Some(message);
            }
        }
        cx.emit(SyncEvent::StatusChanged);
        cx.notify();
    }
}
