//! The OAuth device flow, for a GitHub App or an OAuth App: rc-git asks the host for a user
//! code, the user enters it on `https://<host>/login/device` in their browser, and rc-git
//! polls the token endpoint until the user approves, declines, or the code expires.
//!
//! The flow never opens a page the server names: a `verification_uri` other than the host's
//! own `/login/device` stops it. It polls no faster than `interval`, slows down by what a
//! `slow_down` says, stops at `expires_in` without another request, and every wait looks at
//! the caller's [`Cancel`]. One flow runs per process at a time. The device code is a secret
//! and is never shown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::host::GitHubHost;
use super::oauth::{FormPost, TokenAnswer, post_form, post_token_endpoint};
use super::secret::{ClientId, DeviceCode, TokenSet};
use super::text::single_line;
use super::{GitHubError, GitHubResult};
use crate::actions::remote::{CANCEL_POLL, Cancel};

/// The grant type of a device flow poll.
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The scope an OAuth App asks for: full access to the repositories the user can reach, which
/// merging, re-running and branch deletion need. A GitHub App ignores scopes; its permissions
/// are set on the app.
const OAUTH_SCOPE: &str = "repo";

/// What GitHub waits between polls when it does not say.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// What `slow_down` adds when it names no new interval.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// The bounds a server's interval is held to.
const MIN_INTERVAL: Duration = Duration::from_secs(1);
const MAX_INTERVAL: Duration = Duration::from_secs(120);

/// How long a code lives when the server does not say, and the most rc-git waits for one.
const DEFAULT_EXPIRY: Duration = Duration::from_mins(15);
const MAX_EXPIRY: Duration = Duration::from_hours(1);

/// Set while a [`DeviceFlow`] of this process lives.
static FLOW_RUNNING: AtomicBool = AtomicBool::new(false);

/// This process's one running device flow; released on drop.
#[derive(Debug)]
struct FlowClaim;

impl FlowClaim {
    fn take(host: &GitHubHost) -> GitHubResult<Self> {
        FLOW_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Self)
            .map_err(|_| GitHubError::FlowInProgress {
                host: host.to_string(),
            })
    }
}

impl Drop for FlowClaim {
    fn drop(&mut self) {
        FLOW_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Which kind of app signs in. Both use the device flow and no client secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceMethod {
    /// A GitHub App: fine-grained permissions set on the app, user tokens that expire after
    /// eight hours and renew with a refresh token.
    GitHubApp,
    /// An OAuth App: the `repo` scope, a token that does not expire.
    OAuthApp,
}

/// One answer to [`DeviceFlow::poll`] short of an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user has not entered the code yet.
    Pending,
    /// The host asked rc-git to poll less often; later polls wait `interval`.
    SlowedDown {
        /// The wait between polls from now on.
        interval: Duration,
    },
    /// The user approved; these are the tokens.
    Granted(TokenSet),
}

/// When the next poll may go, and how far apart polls are.
#[derive(Debug)]
struct Pace {
    interval: Duration,
    next_due: Instant,
}

/// A device flow waiting for the user: the code to show, the page to open, and the poll.
/// Dropping it ends the flow for this process, so another may start.
#[derive(Debug)]
pub struct DeviceFlow {
    _claim: FlowClaim,
    host: GitHubHost,
    client: ClientId,
    device_code: DeviceCode,
    user_code: String,
    expires_at: Instant,
    pace: Mutex<Pace>,
}

/// Starts a device flow on `host` for the app `client`: asks the host for a user code and
/// checks the answer. The flow holds this process's one device-flow slot until it is dropped.
///
/// # Errors
/// [`GitHubError::FlowInProgress`] while another flow of this process lives,
/// [`GitHubError::ForeignVerificationUri`] when the answer names a page other than
/// [`GitHubHost::verification_url`], [`GitHubError::OAuth`] for an OAuth error (an unknown
/// client id, device flow disabled on the app), [`GitHubError::Malformed`] for an answer
/// without a well-formed `device_code` and `user_code`, else what the request returns.
pub fn start_device_flow(
    host: &GitHubHost,
    client: &ClientId,
    method: DeviceMethod,
    cancel: &Cancel,
) -> GitHubResult<DeviceFlow> {
    const OP: &str = "start sign-in";
    let claim = FlowClaim::take(host)?;
    let mut form = vec![("client_id", client.as_str())];
    if method == DeviceMethod::OAuthApp {
        form.push(("scope", OAUTH_SCOPE));
    }
    let post = FormPost {
        path: "/login/device/code",
        op: OP,
        fields: &form,
        secrets: Vec::new(),
    };
    let answer = post_form(host, post, cancel)?;
    let value = &answer.value;
    if let Some(code) = value.get("error").and_then(Value::as_str) {
        return Err(GitHubError::OAuth {
            host: host.to_string(),
            code: single_line(code, 80),
            description: answer.text("error_description"),
        });
    }
    let malformed = |reason: &str| GitHubError::Malformed {
        op: OP,
        url: answer.url.clone(),
        reason: reason.to_owned(),
    };
    let device_code = value
        .get("device_code")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("it has no device_code"))?;
    let device_code = DeviceCode::new(device_code.to_owned())
        .map_err(|_| malformed("its device_code is not a code"))?;
    let user_code = value
        .get("user_code")
        .and_then(Value::as_str)
        .filter(|code| is_user_code(code))
        .ok_or_else(|| malformed("it has no user_code of letters, digits and '-'"))?;
    let expected = host.verification_url();
    let uri = value.get("verification_uri").and_then(Value::as_str);
    if uri != Some(expected.as_str()) {
        return Err(GitHubError::ForeignVerificationUri {
            host: host.to_string(),
            uri: single_line(uri.unwrap_or_default(), 200),
            expected,
        });
    }
    let seconds = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
    };
    let interval = seconds("interval")
        .unwrap_or(DEFAULT_INTERVAL)
        .clamp(MIN_INTERVAL, MAX_INTERVAL);
    let lifetime = seconds("expires_in")
        .unwrap_or(DEFAULT_EXPIRY)
        .clamp(MIN_INTERVAL, MAX_EXPIRY);
    let now = Instant::now();
    Ok(DeviceFlow {
        _claim: claim,
        host: host.clone(),
        client: client.clone(),
        device_code,
        user_code: user_code.to_owned(),
        expires_at: now + lifetime,
        pace: Mutex::new(Pace {
            interval,
            next_due: now + interval,
        }),
    })
}

/// Whether `code` looks like a user code: 1 to 32 ASCII letters, digits and dashes.
fn is_user_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 32
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl DeviceFlow {
    /// The host signed into.
    #[must_use]
    pub fn host(&self) -> &GitHubHost {
        &self.host
    }

    /// The code the user enters, e.g. `WDJB-MJHT`.
    #[must_use]
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    /// The page to open: [`GitHubHost::verification_url`], never the server's text.
    #[must_use]
    pub fn verification_url(&self) -> String {
        self.host.verification_url()
    }

    /// The wait between polls now.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.pace().interval
    }

    /// How long the code still lives.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.expires_at.saturating_duration_since(Instant::now())
    }

    fn pace(&self) -> std::sync::MutexGuard<'_, Pace> {
        self.pace.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until the next poll is due, looking at `cancel` every 200 ms (`CANCEL_POLL`), then
    /// asks the token endpoint once. Past the code's expiry it answers without a request.
    ///
    /// # Errors
    /// [`GitHubError::Cancelled`], [`GitHubError::DeviceCodeExpired`] (the code ran out, here
    /// or on the host), [`GitHubError::AccessDenied`] (the user declined), [`GitHubError::OAuth`]
    /// for any other OAuth error, else what the request returns.
    pub fn poll(&self, cancel: &Cancel) -> GitHubResult<DevicePoll> {
        const OP: &str = "finish sign-in";
        let due = self.pace().next_due;
        self.wait_until(due, cancel, OP)?;
        let form = [
            ("client_id", self.client.as_str()),
            ("device_code", self.device_code.expose_secret()),
            ("grant_type", DEVICE_GRANT),
        ];
        let secrets = vec![self.device_code.expose_secret().to_owned()];
        let answer = post_token_endpoint(&self.host, OP, &form, secrets, cancel)?;
        self.judge(answer)
    }

    /// What a poll's answer means, pacing the next poll by it: `interval` after this one, or
    /// the slower interval a `slow_down` asks for.
    fn judge(&self, answer: TokenAnswer) -> GitHubResult<DevicePoll> {
        let mut pace = self.pace();
        pace.next_due = Instant::now() + pace.interval;
        let (code, description, interval) = match answer {
            TokenAnswer::Tokens(tokens) => return Ok(DevicePoll::Granted(tokens)),
            TokenAnswer::Error {
                code,
                description,
                interval,
            } => (code, description, interval),
        };
        let host = self.host.to_string();
        match code.as_str() {
            "authorization_pending" => Ok(DevicePoll::Pending),
            "slow_down" => {
                let slower = interval
                    .map(Duration::from_secs)
                    .filter(|named| *named > pace.interval)
                    .unwrap_or(pace.interval + SLOW_DOWN_STEP)
                    .clamp(MIN_INTERVAL, MAX_INTERVAL);
                pace.interval = slower;
                pace.next_due = Instant::now() + slower;
                Ok(DevicePoll::SlowedDown { interval: slower })
            }
            "expired_token" | "token_expired" => Err(GitHubError::DeviceCodeExpired { host }),
            "access_denied" => Err(GitHubError::AccessDenied { host }),
            _ => Err(GitHubError::OAuth {
                host,
                code,
                description,
            }),
        }
    }

    /// Sleeps until `due` in [`CANCEL_POLL`] steps; refuses once the code has expired.
    fn wait_until(&self, due: Instant, cancel: &Cancel, op: &'static str) -> GitHubResult<()> {
        loop {
            if cancel.is_cancelled() {
                return Err(GitHubError::Cancelled { op });
            }
            let now = Instant::now();
            if now >= self.expires_at {
                return Err(GitHubError::DeviceCodeExpired {
                    host: self.host.to_string(),
                });
            }
            if now >= due {
                return Ok(());
            }
            std::thread::sleep(due.saturating_duration_since(now).min(CANCEL_POLL));
        }
    }
}
