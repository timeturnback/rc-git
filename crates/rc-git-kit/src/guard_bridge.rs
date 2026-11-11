//! Port of the value logic in `RepositoryGuard.swift`: how the window renders a policy
//! decision, the warnings beside an action that landed with its housekeeping incomplete, how
//! a failed guarded action is answered, and the one cancel flag an operation polls. The
//! actions themselves are `rc_git_guard::actions` calls; nothing here writes.
//!
//! [`classify`] replaces the FFI's `action_status` and `finish_remote` status codes with a
//! typed answer.

use std::sync::atomic::{AtomicUsize, Ordering};

use rc_git_guard::actions::{ActionError, Cancel, CheckoutOutcome, Committed};
use rc_git_guard::{Decision, Risk};

pub use rc_git_guard::actions::{Credentials, FileSelection as CommitSelection, HunkStamp};

/// Which way the policy went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionKind {
    /// Run it.
    Allow,
    /// Run it after the user answers the challenge.
    Confirm,
    /// Never run it from rc-git.
    Deny,
}

/// The policy's answer for a proposed action, flattened for a sheet to render. The app
/// renders this; it never classifies an action itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GuardDecision {
    /// Allow, confirm, or deny.
    pub kind: DecisionKind,
    /// How much the action can lose: the challenge's risk on a confirm, safe on an allow,
    /// destructive on a deny.
    pub risk: Risk,
    /// One sentence saying what will happen; empty outside a confirm.
    pub summary: String,
    /// Text the user must type exactly; `None` means a plain confirm button is enough.
    pub typed_phrase: Option<String>,
    /// Whether a backup ref is written before the action runs.
    pub backup_ref: bool,
    /// Why the action is refused. Deny only.
    pub reason: Option<String>,
    /// Whether every commit of the branch is reachable elsewhere. Branch deletion only;
    /// `false` otherwise.
    pub merged: bool,
}

impl GuardDecision {
    /// Flattens `decision`; `merged` comes from a branch-delete plan and is `false` for every
    /// other action.
    #[must_use]
    pub fn new(decision: &Decision, merged: bool) -> Self {
        match decision {
            Decision::Allow => Self {
                kind: DecisionKind::Allow,
                risk: Risk::Safe,
                summary: String::new(),
                typed_phrase: None,
                backup_ref: false,
                reason: None,
                merged,
            },
            Decision::Confirm(challenge) => Self {
                kind: DecisionKind::Confirm,
                risk: challenge.risk(),
                summary: challenge.summary().to_owned(),
                typed_phrase: challenge.typed_phrase().map(str::to_owned),
                backup_ref: challenge.backup_ref(),
                reason: None,
                merged,
            },
            Decision::Deny { reason } => Self {
                kind: DecisionKind::Deny,
                risk: Risk::Destructive,
                summary: String::new(),
                typed_phrase: None,
                backup_ref: false,
                reason: Some(reason.clone()),
                merged,
            },
        }
    }
}

/// What the user should read beside a commit that landed with its housekeeping incomplete,
/// `None` when everything was written.
#[must_use]
pub fn commit_warning(committed: &Committed) -> Option<String> {
    let mut lines = Vec::new();
    if !committed.index_refreshed {
        lines.push(
            "The commit was made, but .git/index was not updated; run \
             `git reset -q -- <paths>` so git status agrees with it.",
        );
    }
    if !committed.reflog_written {
        lines.push(
            "The commit was made, but .git/logs/HEAD did not record it, so HEAD@{1} skips it.",
        );
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// What the user should read beside a switch that landed without its `logs/HEAD` line,
/// `None` when it was written or none was due.
#[must_use]
pub fn checkout_warning(outcome: &CheckoutOutcome) -> Option<String> {
    (!outcome.reflog_written).then(|| {
        format!(
            "Switched to '{}', but .git/logs/HEAD did not record it, so `git checkout -` and \
             HEAD@{{1}} skip the switch.",
            outcome.branch
        )
    })
}

/// Title for an alert about a failed read of the repository.
pub const READ_FAILURE_TITLE: &str = "rc-git could not read the repository";

/// Title for the soft notice after an action found the repository changed under it.
pub const STALE_TITLE: &str = "The repository changed";

/// Title for an alert about a guarded action that failed or was refused, named by the verb
/// its button and challenge carry: "Delete Branch" titles "rc-git could not delete branch".
#[must_use]
pub fn failure_title(verb: &str) -> String {
    format!("rc-git could not {}", verb.to_lowercase())
}

/// What a failed guarded action means for the window, beyond its message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// No author identity: ask for one and retry with it as the fallback.
    IdentityRequired,
    /// The repository changed since the list was built; nothing was written.
    Stale,
    /// The host wants credentials it did not get, or rejected the given ones. Over ssh it
    /// means every key was offered and none accepted: no passphrase can fix that.
    AuthRequired,
    /// The remote redirected to another host, which asked for credentials; no prompt can fix
    /// it.
    CredentialHostRefused,
    /// An encrypted ssh key stayed locked; `key` is the file's path, for the prompt.
    PassphraseRequired {
        /// Path of the encrypted key file.
        key: String,
    },
    /// Local and remote history diverged, or the remote moved past the last fetch; reconcile
    /// in git.
    Diverged,
    /// The ssh host's key is unknown; `host_key` is `"<key type> <fingerprint>"` for the
    /// user to verify before trusting.
    HostKeyUnknown {
        /// `"<key type> <fingerprint>"`.
        host_key: String,
    },
    /// The operation's [`CancelToken`] was signalled.
    Cancelled,
    /// Everything else: show the message.
    Failed,
}

/// The kind of `error`. Exhaustive with no catch-all, so a new [`ActionError`] variant does
/// not compile until someone decides what the window does with it.
#[must_use]
pub fn classify(error: &ActionError) -> FailureKind {
    match error {
        ActionError::MissingIdentity { .. } => FailureKind::IdentityRequired,
        ActionError::StaleRepository { .. } => FailureKind::Stale,
        ActionError::AuthRequired { .. } => FailureKind::AuthRequired,
        ActionError::CredentialHostRefused { .. } => FailureKind::CredentialHostRefused,
        ActionError::PassphraseRequired { key, .. } => {
            FailureKind::PassphraseRequired { key: key.clone() }
        }
        ActionError::Diverged { .. } | ActionError::PushRejected { .. } => FailureKind::Diverged,
        ActionError::UnknownHostKey {
            key_type,
            fingerprint,
            ..
        } => FailureKind::HostKeyUnknown {
            host_key: format!("{key_type} {fingerprint}"),
        },
        ActionError::Cancelled { .. } => FailureKind::Cancelled,
        ActionError::Denied { .. }
        | ActionError::NotConfirmed { .. }
        | ActionError::CheckedOut { .. }
        | ActionError::DetachedHead { .. }
        | ActionError::HunkMismatch { .. }
        | ActionError::NoHunksSelected { .. }
        | ActionError::DirtyPath { .. }
        | ActionError::InvalidIdentity { .. }
        | ActionError::UnsupportedRemote { .. }
        | ActionError::HostKeyMismatch { .. }
        | ActionError::HostKeyRevoked { .. }
        | ActionError::AgentSignRefused { .. }
        | ActionError::NoUpstream { .. }
        | ActionError::UpstreamGone { .. }
        | ActionError::Git { .. } => FailureKind::Failed,
    }
}

/// What the window shows for a failed guarded action.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GuardFailure {
    /// The repository changed under the action: reload, then show `notice` as a soft,
    /// informational sheet titled [`STALE_TITLE`].
    Stale {
        /// The error and what to do next.
        notice: String,
    },
    /// A real failure or refusal: show `message` as an alert.
    Failed {
        /// The error's message.
        message: String,
    },
}

/// How the window answers a failed guarded action. A stale repository is not a failure:
/// nothing was written, so the lists reload and a soft notice says to look again. Every other
/// error is an alert with its message.
#[must_use]
pub fn failure_response(error: &ActionError) -> GuardFailure {
    let message = error.to_string();
    match classify(error) {
        FailureKind::Stale => GuardFailure::Stale {
            notice: format!("{message}\nThe list has been reloaded; check it and try again."),
        },
        FailureKind::IdentityRequired
        | FailureKind::AuthRequired
        | FailureKind::CredentialHostRefused
        | FailureKind::PassphraseRequired { .. }
        | FailureKind::Diverged
        | FailureKind::HostKeyUnknown { .. }
        | FailureKind::Cancelled
        | FailureKind::Failed => GuardFailure::Failed { message },
    }
}

/// The branch or tag name a name sheet hands on: the field without surrounding whitespace,
/// or `None` while nothing is left, which keeps the sheet's Create disabled. Whether git
/// accepts the name is the guard's call, made when the ref is created.
#[must_use]
pub fn entered_ref_name(raw: &str) -> Option<&str> {
    let name = raw.trim();
    (!name.is_empty()).then_some(name)
}

/// A flag the app raises to stop a fetch, a push or a highlight already running. The guard
/// polls [`CancelToken::remote`] between steps and inside the ssh transport's reads, and the
/// highlighter polls [`CancelToken::highlight`] while a parse runs, so a cancel lands within
/// a fraction of a second instead of waiting for the work to end.
///
/// One token belongs to one operation: share it (an `Arc`) with the thread that signals.
#[derive(Debug, Default)]
pub struct CancelToken {
    remote: Cancel,
    /// The same signal in the shape tree-sitter polls, which is a nonzero `AtomicUsize`.
    highlight: AtomicUsize,
}

impl CancelToken {
    /// A fresh flag, nobody having cancelled yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the running operation to stop. Safe from any thread, and the point of the type:
    /// the thread inside the operation is blocked and cannot ask itself.
    pub fn signal(&self) {
        self.remote.signal();
        self.highlight.store(1, Ordering::Relaxed);
    }

    /// Whether [`CancelToken::signal`] has been called, so a caller unwinding an operation
    /// can tell a cancel from a genuine failure.
    #[must_use]
    pub fn is_signalled(&self) -> bool {
        self.remote.is_cancelled()
    }

    /// The flag to pass to a guard remote action (fetch, push, sync).
    #[must_use]
    pub fn remote(&self) -> &Cancel {
        &self.remote
    }

    /// The flag to pass to `rc_git_highlight::highlight` and its fence variant.
    #[must_use]
    pub fn highlight(&self) -> &AtomicUsize {
        &self.highlight
    }
}
