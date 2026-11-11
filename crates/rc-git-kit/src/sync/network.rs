//! One network step with its credential loop: stored credentials (or none) first, then
//! prompt-and-retry on what a secret can fix (`AuthRequired` over HTTPS, `PassphraseRequired`
//! over ssh), storing only what worked. An unknown ssh host key gets one trust prompt,
//! separate from the credential attempts; the accepted fingerprint persists in the host-key
//! store. An upstream that names no host gets one plain attempt and no prompt.

use std::sync::Arc;

use gpui::AsyncApp;
use rc_git_guard::actions::{ActionError, ActionResult, UpstreamStatus};

use super::flow::{Flow, Flowing, StepFailure};
use super::values::{
    CredentialReply, CredentialRetry, HostKeyReply, SyncStep, changed_host_key_message,
    credential_retry, no_prompt_message,
};
use super::{CredentialRequest, HostKeyRequest, WorkflowCredentials};
use crate::blocking::blocking;
use crate::guard_bridge::{CancelToken, Credentials, FailureKind, classify};
use crate::oneshot;
use crate::platform::credentials::CredentialKind;
use crate::repository_actions::{RemoteRequest, RepositoryActions};

/// Authentication attempts one step makes before giving up: the stored (or anonymous)
/// attempt, then up to two prompted ones.
const ATTEMPT_LIMIT: usize = 3;

/// One network attempt: the actions, how to authenticate, and the workflow's cancel flag.
pub(super) type NetworkOperation<T> =
    fn(&dyn RepositoryActions, &RemoteRequest, &CancelToken) -> ActionResult<T>;

/// An unknown host key the guard met: `"<key type> <fingerprint>"`, and the guard's message.
struct Offered {
    host_key: String,
    message: String,
}

/// Where one step's credential loop stands.
struct Attempts {
    host: String,
    kind: CredentialKind,
    credentials: Option<Credentials>,
    accepted_host_key: Option<String>,
    from_prompt: bool,
    host_key_prompted: bool,
    auth_tries: usize,
}

impl Flow {
    /// Runs one network step, negotiating credentials and host keys for the host the last
    /// status names.
    pub(super) async fn network_step<T: Send + 'static>(
        &self,
        step: SyncStep,
        operation: NetworkOperation<T>,
        cx: &mut AsyncApp,
    ) -> Flowing<T> {
        self.enter(step, cx)?;
        let status = self.model(cx, |model, _| model.remote_status.clone())?;
        let Some(status) = status.filter(|status| !status.host.is_empty()) else {
            return self.hostless_step(step, operation, cx).await;
        };
        let mut attempts = self.first_attempt(&status, cx).await?;
        loop {
            // Covers the gap a sheet opens: a cancel raised while the user is looking at a
            // prompt must not start another attempt when they answer it.
            self.bail_if_cancelled(step)?;
            let request = RemoteRequest {
                credentials: attempts.credentials.clone(),
                host: Some(attempts.host.clone()),
                accepted_host_key: attempts.accepted_host_key.clone(),
            };
            let error = match self.attempt(operation, request, cx).await {
                Ok(outcome) => {
                    self.remember_if_prompted(&attempts, cx).await?;
                    return Ok(outcome);
                }
                Err(error) => error,
            };
            self.answer_failure(step, &mut attempts, &error, cx).await?;
        }
    }

    /// The first attempt's state: what the store or this workflow holds for the host, and
    /// the fingerprint approved for it.
    async fn first_attempt(&self, status: &UpstreamStatus, cx: &mut AsyncApp) -> Flowing<Attempts> {
        let host = status.host.clone();
        let kind = if status.uses_ssh {
            CredentialKind::SshPassphrase
        } else {
            CredentialKind::HttpsToken
        };
        let credentials = self.saved_credentials(&host, kind, cx).await?;
        let accepted_host_key = self.host_keys.fingerprint(&host);
        Ok(Attempts {
            host,
            kind,
            credentials,
            accepted_host_key,
            from_prompt: false,
            host_key_prompted: false,
            auth_tries: 0,
        })
    }

    async fn attempt<T: Send + 'static>(
        &self,
        operation: NetworkOperation<T>,
        request: RemoteRequest,
        cx: &mut AsyncApp,
    ) -> ActionResult<T> {
        let cancel = Arc::clone(&self.cancel);
        self.run_blocking(cx, move |actions| operation(actions, &request, &cancel))
            .await
    }

    /// Answers a failed attempt: a host-key prompt for an unknown key, else a credentials
    /// prompt when a secret can fix it; any other failure stops the step.
    async fn answer_failure(
        &self,
        step: SyncStep,
        attempts: &mut Attempts,
        error: &ActionError,
        cx: &mut AsyncApp,
    ) -> Flowing<()> {
        if let FailureKind::HostKeyUnknown { host_key } = classify(error) {
            let offered = Offered {
                host_key,
                message: error.to_string(),
            };
            let trusted = self.trust_host_key(step, attempts, &offered, cx).await?;
            attempts.accepted_host_key = Some(trusted);
            attempts.host_key_prompted = true;
            return Ok(());
        }
        attempts.auth_tries += 1;
        let prompted = self.next_credentials(step, attempts, error, cx).await?;
        attempts.credentials = Some(prompted);
        attempts.from_prompt = true;
        Ok(())
    }

    /// Answers an unknown ssh host key: refuses a key that differs from an earlier approval
    /// and a second prompt in one step, otherwise asks once and stores the fingerprint the
    /// user trusted. Returns that fingerprint for the next attempt.
    async fn trust_host_key(
        &self,
        step: SyncStep,
        attempts: &Attempts,
        offered: &Offered,
        cx: &mut AsyncApp,
    ) -> Flowing<String> {
        let Offered { host_key, message } = offered;
        self.bail_if_cancelled(step)?;
        let fingerprint = host_key
            .split(' ')
            .next_back()
            .unwrap_or(host_key.as_str())
            .to_owned();
        // A key that differs from an earlier approval is a changed key, never a first
        // contact: refuse it without offering the trust sheet.
        if let Some(approved) = &attempts.accepted_host_key
            && *approved != fingerprint
        {
            let message = changed_host_key_message(&attempts.host, approved, &fingerprint);
            return Err(StepFailure::halt(step, message));
        }
        if attempts.host_key_prompted {
            return Err(StepFailure::halt(step, message.as_str()));
        }
        let request = HostKeyRequest {
            host: attempts.host.clone(),
            host_key: host_key.clone(),
        };
        match self.prompt_host_key(request, cx).await? {
            HostKeyReply::Trusted => {}
            HostKeyReply::Refused => return Err(StepFailure::halt(step, "host not trusted")),
            HostKeyReply::Unavailable => {
                return Err(StepFailure::halt(
                    step,
                    format!("{message}; no prompt is available to approve it"),
                ));
            }
        }
        if let Err(error) = self.host_keys.store(&fingerprint, &attempts.host) {
            self.model(cx, |model, _| {
                model.credential_notice =
                    Some(format!("the host key approval was not saved: {error}"));
            })?;
        }
        Ok(fingerprint)
    }

    /// The credentials to retry with after `error`, asked of the user. Fails the step when a
    /// secret cannot fix the error, after [`ATTEMPT_LIMIT`] attempts, or when the prompt is
    /// cancelled or unavailable.
    async fn next_credentials(
        &self,
        step: SyncStep,
        attempts: &Attempts,
        error: &ActionError,
        cx: &mut AsyncApp,
    ) -> Flowing<Credentials> {
        let CredentialRetry::Prompt { key } = credential_retry(error, attempts.kind) else {
            return Err(StepFailure::of(step, error));
        };
        self.bail_if_cancelled(step)?;
        if attempts.auth_tries >= ATTEMPT_LIMIT {
            let message = format!("{error} (gave up after {} attempts)", attempts.auth_tries);
            return Err(StepFailure::halt(step, message));
        }
        let request = CredentialRequest {
            host: attempts.host.clone(),
            kind: attempts.kind,
            key,
        };
        match self.prompt_credentials(request, cx).await? {
            CredentialReply::Provided(prompted) => Ok(prompted),
            CredentialReply::Cancelled => Err(StepFailure::halt(step, "authentication canceled")),
            CredentialReply::Unavailable => Err(StepFailure::halt(
                step,
                no_prompt_message(
                    &attempts.host,
                    attempts.credentials.is_some(),
                    &error.to_string(),
                ),
            )),
        }
    }

    /// The credentials to try first for `host`: the ones this workflow already settled on,
    /// or else the store's, read once off the UI thread (a keychain read can wait on an
    /// access dialog). A failed read is reported with the outcome, and the workflow goes on
    /// without credentials, as it does when nothing is stored, rather than read again.
    async fn saved_credentials(
        &self,
        host: &str,
        kind: CredentialKind,
        cx: &mut AsyncApp,
    ) -> Flowing<Option<Credentials>> {
        let known = self.model(cx, |model, _| model.workflow_credentials.clone())?;
        if let Some(known) = known.filter(|known| known.host == host && known.kind == kind) {
            return Ok(known.credentials);
        }
        let store = Arc::clone(&self.store);
        let asked = host.to_owned();
        let read = blocking(cx.background_executor(), move || {
            store.credentials(&asked, kind)
        })
        .await;
        let (found, notice) = match read {
            Ok(found) => (found, None),
            Err(error) => (
                None,
                Some(format!("the saved credentials could not be read: {error}")),
            ),
        };
        let settled = WorkflowCredentials {
            host: host.to_owned(),
            kind,
            credentials: found.clone(),
        };
        self.model(cx, |model, _| {
            model.workflow_credentials = Some(settled);
            if notice.is_some() {
                model.credential_notice = notice;
            }
        })?;
        Ok(found)
    }

    /// Keeps prompted credentials the server accepted, for the rest of the workflow and in
    /// the store. A failed write is reported; the step that used them still succeeded.
    async fn remember_if_prompted(&self, attempts: &Attempts, cx: &mut AsyncApp) -> Flowing<()> {
        let Some(credentials) = attempts
            .credentials
            .clone()
            .filter(|_| attempts.from_prompt)
        else {
            return Ok(());
        };
        let settled = WorkflowCredentials {
            host: attempts.host.clone(),
            kind: attempts.kind,
            credentials: Some(credentials.clone()),
        };
        self.model(cx, |model, _| model.workflow_credentials = Some(settled))?;
        let store = Arc::clone(&self.store);
        let host = attempts.host.clone();
        let kind = attempts.kind;
        let write = blocking(cx.background_executor(), move || {
            store.store(&credentials, &host, kind)
        })
        .await;
        if let Err(error) = write {
            let secret = match kind {
                CredentialKind::HttpsToken => "token",
                CredentialKind::SshPassphrase => "passphrase",
            };
            let notice = format!("signed in, but the {secret} was not saved: {error}");
            self.model(cx, |model, _| model.credential_notice = Some(notice))?;
        }
        Ok(())
    }

    /// A step whose upstream names no host: a local upstream, whose fetch the guard skips,
    /// or a URL the guard refuses by its scheme. One attempt with no credentials and no
    /// approved host key, and no prompt: a secret or approval kept under an empty host would
    /// be handed to every other upstream that names none, in any repository.
    async fn hostless_step<T: Send + 'static>(
        &self,
        step: SyncStep,
        operation: NetworkOperation<T>,
        cx: &mut AsyncApp,
    ) -> Flowing<T> {
        self.bail_if_cancelled(step)?;
        let error = match self.attempt(operation, RemoteRequest::default(), cx).await {
            Ok(outcome) => return Ok(outcome),
            Err(error) => error,
        };
        match classify(&error) {
            FailureKind::AuthRequired
            | FailureKind::PassphraseRequired { .. }
            | FailureKind::HostKeyUnknown { .. } => Err(StepFailure::halt(
                step,
                "cannot tell which host the upstream is on",
            )),
            FailureKind::IdentityRequired
            | FailureKind::Stale
            | FailureKind::CredentialHostRefused
            | FailureKind::Diverged
            | FailureKind::Cancelled
            | FailureKind::Failed => Err(StepFailure::of(step, &error)),
        }
    }

    /// Asks the window's credentials prompter, or answers unavailable when there is none.
    async fn prompt_credentials(
        &self,
        request: CredentialRequest,
        cx: &mut AsyncApp,
    ) -> Flowing<CredentialReply> {
        let prompter = self.model(cx, |model, _| model.credential_prompter.clone())?;
        let Some(prompter) = prompter else {
            return Ok(CredentialReply::Unavailable);
        };
        let (sender, receiver) = oneshot::channel();
        cx.update(|app| prompter(request, sender, app));
        Ok(receiver.await.unwrap_or(CredentialReply::Unavailable))
    }

    /// Asks the window's host-key prompter, or answers unavailable when there is none.
    async fn prompt_host_key(
        &self,
        request: HostKeyRequest,
        cx: &mut AsyncApp,
    ) -> Flowing<HostKeyReply> {
        let prompter = self.model(cx, |model, _| model.host_key_prompter.clone())?;
        let Some(prompter) = prompter else {
            return Ok(HostKeyReply::Unavailable);
        };
        let (sender, receiver) = oneshot::channel();
        cx.update(|app| prompter(request, sender, app));
        Ok(receiver.await.unwrap_or(HostKeyReply::Unavailable))
    }
}
