//! What the guard and sync sheets say and when their confirm button takes a click
//! (`MainWindowController.presentChallenge`, `IdentitySheet.swift`, `SyncPrompts.swift`, the
//! sync tooltips and the diverged sheet). The sheets render these values; none of them words
//! or decides anything of its own.

use rc_git_guard::Risk;

use crate::commit_identity::{CommitAuthor, CommitIdentity};
use crate::guard_bridge::GuardDecision;
use crate::platform::credentials::CredentialKind;
use crate::sync::{SyncState, SyncStep, idle_sync_tooltip, reconcile_command};

/// The challenge sheet's text: what will happen, the backup ref when one is written, and the
/// phrase to type when the policy names one.
#[must_use]
pub fn challenge_message(decision: &GuardDecision) -> String {
    let mut message = decision.summary.clone();
    if decision.backup_ref {
        message.push_str("\nA backup ref under refs/rc-git/backup/ is written first.");
    }
    if let Some(phrase) = &decision.typed_phrase {
        message.push_str("\nType \u{201c}");
        message.push_str(phrase);
        message.push_str("\u{201d} to confirm.");
    }
    message
}

/// Whether the challenge's confirm button takes a click with `typed` in its field: always
/// without a phrase, and with one only once the field holds it, compared as the guard
/// compares it (surrounding whitespace ignored). The guard verifies again when the action
/// runs; this only keeps a click that would be refused from being offered.
#[must_use]
pub fn phrase_matches(decision: &GuardDecision, typed: &str) -> bool {
    decision
        .typed_phrase
        .as_deref()
        .is_none_or(|phrase| typed.trim() == phrase)
}

/// Whether the challenge's confirm button reads as destructive.
#[must_use]
pub fn is_destructive(decision: &GuardDecision) -> bool {
    decision.risk == Risk::Destructive
}

/// What a refused action's alert says.
#[must_use]
pub fn deny_message(decision: &GuardDecision) -> String {
    decision
        .reason
        .clone()
        .unwrap_or_else(|| "The policy refuses this action.".to_owned())
}

/// The identity sheet's title when a commit or stash finds nobody to record.
pub const IDENTITY_ASK_TITLE: &str = "Who is committing?";

/// The identity sheet's text when a commit or stash finds nobody to record.
pub const IDENTITY_ASK_MESSAGE: &str = "This repository\u{2019}s own config sets no \
     user.name/user.email, and rc-git never reads your global git config. Enter the identity \
     to record; it is saved for this repository only, and only once the commit succeeds.";

/// The identity sheet's title from Repository > Commit Identity.
pub const IDENTITY_EDIT_TITLE: &str = "Commit Identity";

/// The identity sheet's text from Repository > Commit Identity, naming the config's identity
/// when it sets one, since that one wins.
#[must_use]
pub fn identity_edit_message(author: Option<&CommitAuthor>) -> String {
    let mut message = "Saved for this repository only, and used only when its own config sets \
                       no user.name/user.email."
        .to_owned();
    if let Some(CommitAuthor::Configured(identity)) = author {
        message.push_str("\nIts config sets ");
        message.push_str(&identity.display());
        message.push_str(" now, so commits use that.");
    }
    message
}

/// What the identity sheet's fields hold: the identity Save hands on, or the problem shown
/// under the fields. A sheet the user has not typed in yet shows no problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityEntry {
    /// The identity to save; `None` keeps Save disabled.
    pub identity: Option<CommitIdentity>,
    /// The line under the fields.
    pub problem: Option<&'static str>,
}

/// Checks the identity sheet's fields.
#[must_use]
pub fn identity_entry(name: &str, email: &str) -> IdentityEntry {
    let untouched = name.is_empty() && email.is_empty();
    match CommitIdentity::parse(name, email) {
        Ok(identity) => IdentityEntry {
            identity: Some(identity),
            problem: None,
        },
        Err(problem) => IdentityEntry {
            identity: None,
            problem: (!untouched).then(|| problem.message()),
        },
    }
}

/// The credentials sheet's words for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialCopy {
    /// The title.
    pub title: String,
    /// The text, led by what was missing from the last confirm.
    pub message: String,
    /// The secret field's placeholder.
    pub secret_placeholder: &'static str,
    /// The confirm button.
    pub confirm: &'static str,
    /// Whether a username field shows.
    pub asks_username: bool,
}

/// The credentials sheet for `host`: an HTTPS token, or the passphrase of the ssh key at
/// `key`. `problem` is what the last confirm left out.
#[must_use]
pub fn credential_copy(
    kind: CredentialKind,
    host: &str,
    key: Option<&str>,
    problem: Option<&str>,
) -> CredentialCopy {
    let (title, message, secret_placeholder, confirm) = match kind {
        CredentialKind::HttpsToken => (
            format!("Sign in to {host}"),
            "rc-git talks to HTTPS remotes with a personal access token. The token is \
             remembered in your login keychain once the server accepts it."
                .to_owned(),
            "Token",
            "Sign In",
        ),
        CredentialKind::SshPassphrase => (
            format!("Unlock the ssh key for {host}"),
            format!(
                "The ssh key {} is encrypted, or the passphrase given was wrong. The \
                 passphrase is remembered in your login keychain once the server accepts the \
                 key.",
                key.map_or_else(|| "for this remote".to_owned(), |key| format!("'{key}'"))
            ),
            "Passphrase",
            "Unlock",
        ),
    };
    let message = match problem {
        Some(problem) => format!("{problem}\n\n{message}"),
        None => message,
    };
    CredentialCopy {
        title,
        message,
        secret_placeholder,
        confirm,
        asks_username: kind == CredentialKind::HttpsToken,
    }
}

/// The host-key sheet's title and text for an unknown ssh host.
#[must_use]
pub fn host_key_copy(host: &str, host_key: &str) -> (String, String) {
    (
        format!("Trust {host}?"),
        format!(
            "This host is not in known_hosts. Its key is:\n\n{host_key}\n\nVerify the \
             fingerprint through a trusted channel before continuing. rc-git remembers the \
             approval for this host; known_hosts is never modified."
        ),
    )
}

/// What the running step is doing, for the tooltip.
#[must_use]
pub fn running_verb(step: SyncStep) -> &'static str {
    match step {
        SyncStep::Checkout => "Switching branch",
        SyncStep::Fetch => "Fetching",
        SyncStep::Pull => "Pulling",
        SyncStep::Push => "Pushing",
    }
}

/// The Sync control's tooltip in `state`: the idle one from the last status, the running
/// step with the way to stop it, or the step being cancelled.
#[must_use]
pub fn sync_tooltip(
    state: &SyncState,
    status: Option<&rc_git_guard::actions::UpstreamStatus>,
    error: Option<&str>,
) -> String {
    match state {
        SyncState::Running(step) => format!("{} \u{2014} click to cancel", running_verb(*step)),
        SyncState::Cancelling(step) => format!(
            "Cancelling the {}\u{2026}",
            running_verb(*step).to_lowercase()
        ),
        SyncState::Idle
        | SyncState::Done(_)
        | SyncState::Diverged(_)
        | SyncState::Failed { .. } => idle_sync_tooltip(status, error),
    }
}

/// The Back-to-default control's tooltip.
#[must_use]
pub fn back_to_default_tooltip(title: &str) -> String {
    format!("{title} \u{2014} switch, fetch, and fast-forward")
}

/// The alert title for a failed sync step.
pub const SYNC_FAILURE_TITLE: &str = "rc-git could not sync";

/// The diverged sheet's title.
pub const DIVERGED_TITLE: &str = "Branch and upstream have diverged";

/// The diverged sheet's text: rc-git only fast-forwards, so it names the command that
/// reconciles in git, in the checkout at `work_dir`. A bare repository has none to pull into.
#[must_use]
pub fn diverged_message(message: &str, work_dir: Option<&str>) -> String {
    match work_dir {
        Some(work_dir) => format!(
            "{message}\n\nrc-git only fast-forwards. Rebase in a terminal, then sync \
             again:\n\n{}",
            reconcile_command(work_dir)
        ),
        None => format!("{message}\n\nReconcile in git (merge or rebase), then sync again."),
    }
}

#[cfg(test)]
mod tests {
    use rc_git_guard::Risk;

    use super::*;
    use crate::guard_bridge::DecisionKind;

    fn confirm(phrase: Option<&str>, backup: bool) -> GuardDecision {
        GuardDecision {
            kind: DecisionKind::Confirm,
            risk: Risk::Destructive,
            summary: "Delete branch 'x'.".to_owned(),
            typed_phrase: phrase.map(str::to_owned),
            backup_ref: backup,
            reason: None,
            merged: false,
        }
    }

    #[test]
    fn the_challenge_names_the_backup_and_the_phrase() {
        assert_eq!(
            challenge_message(&confirm(None, false)),
            "Delete branch 'x'."
        );
        assert_eq!(
            challenge_message(&confirm(Some("x"), true)),
            "Delete branch 'x'.\nA backup ref under refs/rc-git/backup/ is written first.\n\
             Type \u{201c}x\u{201d} to confirm."
        );
    }

    #[test]
    fn a_phrase_must_match_as_the_guard_compares_it() {
        let decision = confirm(Some("delete x"), false);
        assert!(!phrase_matches(&decision, ""));
        assert!(!phrase_matches(&decision, "delete"));
        assert!(!phrase_matches(&decision, "Delete x"));
        assert!(phrase_matches(&decision, "delete x"));
        assert!(phrase_matches(&decision, "  delete x\n"));
        assert!(phrase_matches(&confirm(None, false), ""));
        assert!(is_destructive(&decision));
    }

    #[test]
    fn a_denial_falls_back_to_a_generic_reason() {
        let mut decision = confirm(None, false);
        assert_eq!(deny_message(&decision), "The policy refuses this action.");
        decision.reason = Some("protected".into());
        assert_eq!(deny_message(&decision), "protected");
    }

    #[test]
    fn the_edit_message_names_a_configured_identity() {
        let identity = CommitIdentity {
            name: "A".into(),
            email: "a@b".into(),
        };
        assert!(!identity_edit_message(None).contains("Its config"));
        let configured = CommitAuthor::Configured(identity);
        assert!(
            identity_edit_message(Some(&configured)).ends_with("A <a@b> now, so commits use that.")
        );
    }

    #[test]
    fn an_untouched_identity_sheet_shows_no_problem() {
        assert_eq!(
            identity_entry("", ""),
            IdentityEntry {
                identity: None,
                problem: None
            }
        );
        let partial = identity_entry("A", "");
        assert_eq!(partial.identity, None);
        assert!(partial.problem.is_some());
        let whole = identity_entry(" A ", "a@b");
        assert_eq!(
            whole.identity.map(|identity| identity.name),
            Some("A".into())
        );
        assert_eq!(whole.problem, None);
    }

    #[test]
    fn credential_sheets_differ_by_kind() {
        let https = credential_copy(CredentialKind::HttpsToken, "h", None, None);
        assert_eq!(https.title, "Sign in to h");
        assert!(https.asks_username);
        assert_eq!(https.confirm, "Sign In");
        let ssh = credential_copy(
            CredentialKind::SshPassphrase,
            "h",
            Some("/k"),
            Some("Enter the passphrase."),
        );
        assert!(!ssh.asks_username);
        assert!(
            ssh.message
                .starts_with("Enter the passphrase.\n\nThe ssh key '/k'")
        );
        assert_eq!(ssh.secret_placeholder, "Passphrase");
    }

    #[test]
    fn the_tooltip_follows_the_state() {
        assert_eq!(
            sync_tooltip(&SyncState::Running(SyncStep::Fetch), None, None),
            "Fetching \u{2014} click to cancel"
        );
        assert_eq!(
            sync_tooltip(&SyncState::Cancelling(SyncStep::Push), None, None),
            "Cancelling the pushing\u{2026}"
        );
        assert_eq!(sync_tooltip(&SyncState::Idle, None, None), "Sync");
        assert_eq!(
            back_to_default_tooltip("Back to main"),
            "Back to main \u{2014} switch, fetch, and fast-forward"
        );
    }

    #[test]
    fn the_diverged_sheet_names_the_command_only_with_a_checkout() {
        let with = diverged_message("diverged", Some("/w"));
        assert!(with.ends_with("cd /w && git pull --rebase"));
        assert!(diverged_message("diverged", None).ends_with("then sync again."));
        let (title, message) = host_key_copy("h", "ssh-ed25519 SHA256:x");
        assert_eq!(title, "Trust h?");
        assert!(message.contains("SHA256:x"));
    }
}
