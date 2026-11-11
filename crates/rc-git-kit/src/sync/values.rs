//! The value half of `SyncModel.swift`: which controls can run, their titles, the workflow's
//! steps, states and summaries, the prompt replies, and the decisions the credential loop
//! makes. Pure and tested without a model.

use rc_git_guard::actions::{ActionError, CheckoutOutcome, PushOutcome, UpstreamStatus};

use crate::guard_bridge::{Credentials, FailureKind, classify};
use crate::platform::credentials::CredentialKind;
use crate::shell_word;

/// Which sync controls can run now. The menu, the toolbar and the Changes button row all read
/// this, so a control one surface disables is never clickable on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per control, as the Swift struct has"
)]
pub struct SyncAvailability {
    /// Fetch, Pull, Push and Sync each re-read the upstream first and refuse without one, so
    /// all four need a known upstream: false before the first status, after a failed read,
    /// without an upstream, and while a workflow runs.
    pub can_fetch: bool,
    /// See [`Self::can_fetch`].
    pub can_pull: bool,
    /// See [`Self::can_fetch`].
    pub can_push: bool,
    /// See [`Self::can_fetch`].
    pub can_sync: bool,
    /// The switch is local and the branch switched to decides the fetch, so no upstream
    /// check; a bare repository has no working tree to switch.
    pub can_back_to_default: bool,
    /// A running fetch or push can still be asked to stop.
    pub can_cancel: bool,
}

/// What [`SyncAvailability::new`] decides from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent fact about the window"
)]
pub struct SyncConditions<'a> {
    /// The last status read, `None` before the first one and after a failed one.
    pub status: Option<&'a UpstreamStatus>,
    /// A workflow is running or cancelling.
    pub busy: bool,
    /// The running step takes a cancel.
    pub cancellable: bool,
    /// The window holds a repository and its sync model.
    pub has_repository: bool,
    /// The repository has a checkout (it is not bare).
    pub has_working_tree: bool,
}

impl SyncAvailability {
    /// The controls `conditions` allow.
    #[must_use]
    pub fn new(conditions: SyncConditions<'_>) -> Self {
        let idle = conditions.has_repository && !conditions.busy;
        let remote = idle && conditions.status.is_some_and(|status| status.has_upstream);
        Self {
            can_fetch: remote,
            can_pull: remote,
            can_push: remote,
            can_sync: remote,
            can_back_to_default: idle && conditions.has_working_tree,
            can_cancel: conditions.has_repository && conditions.busy && conditions.cancellable,
        }
    }

    /// The toolbar Sync button becomes Cancel while a workflow runs.
    #[must_use]
    pub fn sync_button_enabled(&self) -> bool {
        self.can_sync || self.can_cancel
    }
}

fn counted(verb: &str, count: usize) -> String {
    match count {
        0 => verb.to_owned(),
        1 => format!("{verb} 1 Commit"),
        count => format!("{verb} {count} Commits"),
    }
}

/// The Pull control's title: the bare verb, or the verb with the count from the last status.
#[must_use]
pub fn pull_title(status: Option<&UpstreamStatus>) -> String {
    counted("Pull", status.map_or(0, |status| status.behind))
}

/// The Push control's title: the bare verb, or the verb with the count from the last status.
#[must_use]
pub fn push_title(status: Option<&UpstreamStatus>) -> String {
    counted("Push", status.map_or(0, |status| status.ahead))
}

/// "Sync — 2 to push, 3 to pull" while idle, from the last status; when the last read
/// failed, says so with the cause rather than repeating an older answer. A branch the
/// upstream has never seen reads "not published", since its 0 ahead counts nothing.
#[must_use]
pub fn idle_sync_tooltip(status: Option<&UpstreamStatus>, error: Option<&str>) -> String {
    if let Some(error) = error {
        return format!("Sync — couldn't read upstream status: {error}");
    }
    let Some(status) = status.filter(|status| status.has_upstream) else {
        return "Sync".to_owned();
    };
    if !status.published {
        return "Sync — not published".to_owned();
    }
    if status.ahead == 0 && status.behind == 0 {
        return "Sync — up to date".to_owned();
    }
    let mut parts = Vec::new();
    if status.ahead > 0 {
        parts.push(format!("{} to push", status.ahead));
    }
    if status.behind > 0 {
        parts.push(format!("{} to pull", status.behind));
    }
    format!("Sync — {}", parts.join(", "))
}

/// Title for the control that switches to the mainline branch and brings it up to date.
#[must_use]
pub fn back_to_default_title(branch: Option<&str>) -> String {
    branch.map_or_else(
        || "Back to the Default Branch".to_owned(),
        |branch| format!("Back to {branch}"),
    )
}

/// The shell command that reconciles a diverged branch. rc-git only fast-forwards, so the
/// rebase runs in git; the `cd` lets the command paste into any shell and leaves that shell
/// in the checkout when the rebase stops on a conflict.
#[must_use]
pub fn reconcile_command(work_dir: &str) -> String {
    format!("cd {} && git pull --rebase", shell_word::quote(work_dir))
}

/// What the user should read beside a push that landed without moving its tracking ref,
/// `None` when it moved.
#[must_use]
pub fn push_warning(outcome: &PushOutcome) -> Option<String> {
    outcome
        .tracking_ref_error
        .as_ref()
        .map(|error| format!("push landed, but the tracking ref was not moved: {error}"))
}

/// What the user should read beside a switch that landed without its reflog line.
#[must_use]
pub fn checkout_warning(outcome: &CheckoutOutcome) -> Option<String> {
    crate::guard_bridge::checkout_warning(outcome)
}

/// One step of a workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncStep {
    /// Switching to the mainline branch.
    Checkout,
    /// Fetching from the upstream.
    Fetch,
    /// Fast-forwarding to the tracking ref.
    Pull,
    /// Pushing to the upstream.
    Push,
}

/// What one completed workflow did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SyncSummary {
    /// Refs the fetch created or moved.
    pub fetched_refs: usize,
    /// Files the switch and the pull wrote.
    pub pulled_files: usize,
    /// Commits the push sent.
    pub pushed_commits: usize,
    /// The step after which a cancel stopped the workflow, `None` when it ran to the end.
    /// Set when the cancel landed during a step that cannot stop (checkout, pull): that step
    /// finished, and what it did is still reported.
    pub cancelled_after: Option<SyncStep>,
    /// What went wrong beside the transfer: the saved credentials could not be read, the
    /// ones that just worked could not be saved, a branch switch landed without its reflog
    /// line, or a push landed without moving its tracking ref. `None` when nothing did.
    pub notice: Option<String>,
}

/// A finished workflow's message for the user: a problem the sync survived, or why a cancel
/// left a finished step in place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyncReport {
    /// The sheet's title.
    pub title: String,
    /// The sheet's text.
    pub message: String,
    /// A problem warns; a cancel explanation only informs.
    pub is_problem: bool,
}

impl SyncSummary {
    /// This summary with `extra` added after any notice it already carries.
    #[must_use]
    pub fn adding_notice(mut self, extra: Option<&str>) -> Self {
        let joined: Vec<&str> = [self.notice.as_deref(), extra]
            .into_iter()
            .flatten()
            .collect();
        self.notice = (!joined.is_empty()).then(|| joined.join("\n"));
        self
    }

    /// What the user is told once the workflow is done, or `None` when it ran to the end
    /// with nothing to report. A notice is a problem; a cancel that stopped after a finished
    /// step is only explained, since that step's work stands.
    #[must_use]
    pub fn report(&self) -> Option<SyncReport> {
        let cancel_note = self.cancelled_after.map(cancel_note);
        if let Some(notice) = &self.notice {
            let parts: Vec<&str> = [cancel_note, Some(notice.as_str())]
                .into_iter()
                .flatten()
                .collect();
            return Some(SyncReport {
                title: "rc-git synced, with a problem".to_owned(),
                message: parts.join("\n\n"),
                is_problem: true,
            });
        }
        cancel_note.map(|note| SyncReport {
            title: "Sync cancelled".to_owned(),
            message: note.to_owned(),
            is_problem: false,
        })
    }
}

/// Why a cancel did not undo `step`: it finished before the cancel could stop it.
fn cancel_note(step: SyncStep) -> &'static str {
    match step {
        SyncStep::Checkout => {
            "The branch switch had already finished, so it stands; nothing was fetched or pulled."
        }
        SyncStep::Fetch => {
            "The fetch had already finished, so its refs stand; nothing after it ran."
        }
        SyncStep::Pull => {
            "The pull had already finished, so its changes stand; nothing was pushed."
        }
        SyncStep::Push => "The push had already finished, so it stands.",
    }
}

/// Where the sync workflow stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum SyncState {
    /// Nothing running, nothing to report.
    #[default]
    Idle,
    /// A step is running.
    Running(SyncStep),
    /// Stopping: the user asked, the step has been told, and the workflow is unwinding.
    Cancelling(SyncStep),
    /// The workflow finished.
    Done(SyncSummary),
    /// Local and upstream both moved; informative and terminal.
    Diverged(String),
    /// A step failed; the message says why.
    Failed {
        /// The step that failed.
        step: SyncStep,
        /// Why.
        message: String,
    },
}

impl SyncState {
    /// Whether a workflow is running or cancelling.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        match self {
            Self::Running(_) | Self::Cancelling(_) => true,
            Self::Idle | Self::Done(_) | Self::Diverged(_) | Self::Failed { .. } => false,
        }
    }

    /// Whether the running workflow can still be asked to stop. Not during a checkout or a
    /// pull: those writes take no cancel and always run to completion.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        match self {
            Self::Running(SyncStep::Fetch | SyncStep::Push) => true,
            Self::Running(SyncStep::Checkout | SyncStep::Pull)
            | Self::Idle
            | Self::Cancelling(_)
            | Self::Done(_)
            | Self::Diverged(_)
            | Self::Failed { .. } => false,
        }
    }
}

/// What a credentials prompt answered: a secret to retry with, a user who declined, or no way
/// to ask at all (no prompter, no window to put the sheet on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialReply {
    /// Retry with these.
    Provided(Credentials),
    /// The user declined.
    Cancelled,
    /// Nobody could be asked.
    Unavailable,
}

/// What an unknown-host-key prompt answered, with the same split between a user who refused
/// and nobody to ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostKeyReply {
    /// Trust the key and retry.
    Trusted,
    /// The user refused it.
    Refused,
    /// Nobody could be asked.
    Unavailable,
}

/// What the credentials sheet holds when the user confirms it: credentials to try, or what
/// is missing, which the sheet shows before asking again. An HTTPS token needs a username;
/// an ssh passphrase has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialEntry {
    /// Try these.
    Valid(Credentials),
    /// Show this and ask again.
    Invalid(&'static str),
}

impl CredentialEntry {
    /// Checks what the sheet holds for `kind`.
    #[must_use]
    pub fn check(kind: CredentialKind, username: &str, secret: &str) -> Self {
        let username = username.trim_matches(' ');
        match kind {
            CredentialKind::HttpsToken if username.is_empty() => {
                return Self::Invalid("Enter a username.");
            }
            CredentialKind::HttpsToken if secret.is_empty() => {
                return Self::Invalid("Enter a token.");
            }
            CredentialKind::SshPassphrase if secret.is_empty() => {
                return Self::Invalid("Enter the passphrase.");
            }
            CredentialKind::HttpsToken | CredentialKind::SshPassphrase => {}
        }
        Self::Valid(Credentials {
            username: username.to_owned(),
            token: secret.to_owned(),
        })
    }
}

/// What a failed attempt allows: a prompt for the secret that can fix it, naming the locked
/// key over ssh, or nothing. Over ssh only a locked key is worth a prompt; a plain rejection
/// means no key was accepted, which no passphrase changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CredentialRetry {
    /// Ask for the secret; `key` names the locked key file over ssh.
    Prompt {
        /// The locked key's path, `None` over HTTPS.
        key: Option<String>,
    },
    /// No secret can fix it.
    None,
}

/// What `error` allows over a transport of `kind`.
#[must_use]
pub fn credential_retry(error: &ActionError, kind: CredentialKind) -> CredentialRetry {
    match (classify(error), kind) {
        (FailureKind::AuthRequired, CredentialKind::HttpsToken) => {
            CredentialRetry::Prompt { key: None }
        }
        (FailureKind::PassphraseRequired { key }, CredentialKind::SshPassphrase) => {
            CredentialRetry::Prompt { key: Some(key) }
        }
        (
            FailureKind::AuthRequired | FailureKind::PassphraseRequired { .. },
            CredentialKind::SshPassphrase | CredentialKind::HttpsToken,
        )
        | (
            FailureKind::IdentityRequired
            | FailureKind::Stale
            | FailureKind::CredentialHostRefused
            | FailureKind::Diverged
            | FailureKind::HostKeyUnknown { .. }
            | FailureKind::Cancelled
            | FailureKind::Failed,
            _,
        ) => CredentialRetry::None,
    }
}

/// Why a step that needed a secret stopped when nobody could be asked for one: nothing was
/// stored, or what was tried failed. The guard's reason rides along either way.
#[must_use]
pub fn no_prompt_message(host: &str, had_credentials: bool, reason: &str) -> String {
    if had_credentials {
        format!("{reason}; no prompt is available to ask again")
    } else {
        format!("credentials required for '{host}' and none are stored: {reason}")
    }
}

/// The refusal for a branch with no upstream, worded as the guard's own `NoUpstream`, so it
/// reads the same from either layer.
#[must_use]
pub fn no_upstream_message(branch: &str) -> String {
    format!(
        "branch '{branch}' has no remote to use; add an 'origin' remote or set \
         branch.{branch}.remote"
    )
}

/// The refusal for a host key that differs from the one the user approved.
#[must_use]
pub fn changed_host_key_message(host: &str, approved: &str, offered: &str) -> String {
    format!(
        "the ssh host key for '{host}' changed: approved {approved}, offered {offered}; this \
         may be a man-in-the-middle — investigate with ssh directly"
    )
}
