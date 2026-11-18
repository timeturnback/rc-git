//! The sign-in sheet's state machine: what the user typed becomes a [`SignInStart`], and each
//! answer the front end brings back (a started device flow, a poll, the token check, the save)
//! moves the flow one step and names the next thing to run. Every answer carries the
//! generation it was asked under, so an answer that lands after a cancel or a restart is
//! dropped. Cancel is offered in every phase until the sign-in is saved.
//!
//! The flow owns the running [`DeviceFlow`], which holds the process's one device-flow slot,
//! and keeps it only while the next step is a poll: every other step (a failure, a cancel, the
//! token check) drops it, so the user can start again from the same sheet.

use std::sync::Arc;
use std::time::Duration;

use rc_git_guard::actions::github::{
    ApiToken, ClientId, DeviceFlow, DeviceMethod, DevicePoll, GitHubError, GitHubHost,
    TokenIdentity, TokenSet,
};

use super::accounts::{GitHubAccount, SignInMethod};
use crate::platform::api_tokens::AccountKey;

/// What the form asks the flow to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInStart {
    /// A device flow for the app `client` on `host`.
    Device {
        /// The host.
        host: GitHubHost,
        /// GitHub App or OAuth App.
        method: DeviceMethod,
        /// The app's client id.
        client: ClientId,
    },
    /// Check and save a pasted token for `host`.
    Token {
        /// The host.
        host: GitHubHost,
        /// The token.
        token: ApiToken,
    },
}

impl SignInStart {
    /// Reads the form: the host, the method, and the client id (device methods) or the token
    /// (the token method). The token is taken exactly as given apart from surrounding
    /// whitespace.
    ///
    /// # Errors
    /// One sentence for the sheet naming the first field that is wrong.
    pub fn from_form(
        host: &str,
        method: SignInMethod,
        client_id: &str,
        token: &str,
    ) -> Result<Self, String> {
        let host = GitHubHost::parse(host).map_err(|error| error.to_string())?;
        let device = match method {
            SignInMethod::GitHubApp => DeviceMethod::GitHubApp,
            SignInMethod::OAuthApp => DeviceMethod::OAuthApp,
            SignInMethod::Token => {
                let token = ApiToken::new(token.trim().to_owned())
                    .map_err(|_| "Paste the whole token: it has no spaces or line breaks.")?;
                return Ok(Self::Token { host, token });
            }
        };
        if client_id.trim().is_empty() {
            return Err("Enter the app's client ID, from its settings page on GitHub.".to_owned());
        }
        let client = ClientId::parse(client_id).map_err(|error| error.to_string())?;
        Ok(Self::Device {
            host,
            method: device,
            client,
        })
    }

    /// The host signed into.
    #[must_use]
    pub fn host(&self) -> &GitHubHost {
        match self {
            Self::Device { host, .. } | Self::Token { host, .. } => host,
        }
    }

    fn method(&self) -> SignInMethod {
        match self {
            Self::Device {
                method: DeviceMethod::GitHubApp,
                ..
            } => SignInMethod::GitHubApp,
            Self::Device {
                method: DeviceMethod::OAuthApp,
                ..
            } => SignInMethod::OAuthApp,
            Self::Token { .. } => SignInMethod::Token,
        }
    }

    fn client(&self) -> Option<ClientId> {
        match self {
            Self::Device { client, .. } => Some(client.clone()),
            Self::Token { .. } => None,
        }
    }
}

/// Where the sign-in is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInPhase {
    /// The form, with the last problem or cancel note when there is one.
    Form {
        /// Shown above the buttons.
        note: Option<String>,
    },
    /// Asking the host for a user code.
    RequestingCode,
    /// The code is on screen; polling until the user enters it.
    WaitingForUser {
        /// The code to enter.
        user_code: String,
        /// The page to enter it on, `https://<host>/login/device`.
        verification_url: String,
        /// Set once the host asked for slower polling.
        slowed_to: Option<Duration>,
    },
    /// Asking the host who the token belongs to.
    Checking,
    /// Saving the token and the account.
    Saving,
    /// Done.
    SignedIn {
        /// The account saved.
        account: GitHubAccount,
    },
}

/// The work the front end runs next, under the generation it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInStep {
    /// Start the device flow (`start_device_flow`) and answer [`SignInFlow::code_shown`] with
    /// it.
    RequestCode {
        /// The generation to answer under.
        generation: u64,
    },
    /// Poll [`SignInFlow::device`] once (`DeviceFlow::poll`, which waits for its interval) and
    /// answer [`SignInFlow::polled`].
    Poll {
        /// The generation to answer under.
        generation: u64,
    },
    /// Check the token (`validate_token`) and answer [`SignInFlow::checked`].
    Check {
        /// The generation to answer under.
        generation: u64,
        /// The token to check.
        token: ApiToken,
    },
    /// Save the tokens and the account and answer [`SignInFlow::saved`].
    Save {
        /// The generation to answer under.
        generation: u64,
        /// The account record.
        account: GitHubAccount,
        /// The tokens.
        tokens: TokenSet,
    },
    /// Nothing to run.
    Idle,
}

/// The sign-in state machine.
#[derive(Debug)]
pub struct SignInFlow {
    phase: SignInPhase,
    generation: u64,
    start: Option<SignInStart>,
    /// The tokens between the grant and the save.
    tokens: Option<TokenSet>,
    /// The running device flow, held only while the phase is
    /// [`SignInPhase::WaitingForUser`].
    device: Option<Arc<DeviceFlow>>,
}

impl Default for SignInFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl SignInFlow {
    /// A flow showing the empty form.
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: SignInPhase::Form { note: None },
            generation: 0,
            start: None,
            tokens: None,
            device: None,
        }
    }

    /// Where the flow is.
    #[must_use]
    pub fn phase(&self) -> &SignInPhase {
        &self.phase
    }

    /// The device flow a [`SignInStep::Poll`] polls; `None` in every other phase.
    #[must_use]
    pub fn device(&self) -> Option<Arc<DeviceFlow>> {
        self.device.clone()
    }

    /// Moves to `phase`, dropping the device flow unless the user is still to enter its code:
    /// the flow's slot is free again as soon as no poll follows.
    fn enter(&mut self, phase: SignInPhase) {
        if !matches!(phase, SignInPhase::WaitingForUser { .. }) {
            self.device = None;
        }
        self.phase = phase;
    }

    /// Whether something runs that Cancel stops.
    #[must_use]
    pub fn is_running(&self) -> bool {
        match self.phase {
            SignInPhase::RequestingCode
            | SignInPhase::WaitingForUser { .. }
            | SignInPhase::Checking
            | SignInPhase::Saving => true,
            SignInPhase::Form { .. } | SignInPhase::SignedIn { .. } => false,
        }
    }

    /// Starts what the form asked for. Ignored unless the form is on screen.
    pub fn submit(&mut self, start: SignInStart) -> SignInStep {
        if !matches!(self.phase, SignInPhase::Form { .. }) {
            return SignInStep::Idle;
        }
        self.generation += 1;
        let generation = self.generation;
        let step = match &start {
            SignInStart::Device { .. } => {
                self.enter(SignInPhase::RequestingCode);
                SignInStep::RequestCode { generation }
            }
            SignInStart::Token { token, .. } => {
                self.enter(SignInPhase::Checking);
                self.tokens = Some(TokenSet::lasting(token.clone()));
                SignInStep::Check {
                    generation,
                    token: token.clone(),
                }
            }
        };
        self.start = Some(start);
        step
    }

    /// Whether Cancel stops something now: every running phase but the save, which is two
    /// local writes that land together or not at all.
    #[must_use]
    pub fn can_cancel(&self) -> bool {
        self.is_running() && self.phase != SignInPhase::Saving
    }

    /// Stops whatever runs and shows the form again with a note. Answers whether anything was
    /// stopped, so the caller knows to signal its cancel token. The save is not stopped.
    pub fn cancel(&mut self) -> bool {
        let running = self.can_cancel();
        if running {
            self.generation += 1;
            self.tokens = None;
            self.enter(SignInPhase::Form {
                note: Some("Sign-in cancelled.".to_owned()),
            });
        }
        running
    }

    fn current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    fn fail(&mut self, message: String) -> SignInStep {
        self.generation += 1;
        self.tokens = None;
        self.enter(SignInPhase::Form {
            note: Some(message),
        });
        SignInStep::Idle
    }

    /// The device flow started, or failed to. A flow that lands after a cancel or a restart
    /// is dropped here, freeing the process's one device-flow slot.
    pub fn code_shown(
        &mut self,
        generation: u64,
        answer: Result<DeviceFlow, GitHubError>,
    ) -> SignInStep {
        if !self.current(generation) || self.phase != SignInPhase::RequestingCode {
            return SignInStep::Idle;
        }
        match answer {
            Ok(device) => {
                self.enter(SignInPhase::WaitingForUser {
                    user_code: device.user_code().to_owned(),
                    verification_url: device.verification_url(),
                    slowed_to: None,
                });
                self.device = Some(Arc::new(device));
                SignInStep::Poll { generation }
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// One poll answered.
    pub fn polled(
        &mut self,
        generation: u64,
        answer: Result<DevicePoll, GitHubError>,
    ) -> SignInStep {
        if !self.current(generation) {
            return SignInStep::Idle;
        }
        let SignInPhase::WaitingForUser { slowed_to, .. } = &mut self.phase else {
            return SignInStep::Idle;
        };
        match answer {
            Ok(DevicePoll::Pending) => SignInStep::Poll { generation },
            Ok(DevicePoll::SlowedDown { interval }) => {
                *slowed_to = Some(interval);
                SignInStep::Poll { generation }
            }
            Ok(DevicePoll::Granted(tokens)) => {
                let token = tokens.access.clone();
                self.tokens = Some(tokens);
                self.enter(SignInPhase::Checking);
                SignInStep::Check { generation, token }
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// The token check answered: who the token belongs to.
    pub fn checked(
        &mut self,
        generation: u64,
        answer: Result<TokenIdentity, GitHubError>,
    ) -> SignInStep {
        if !self.current(generation) || self.phase != SignInPhase::Checking {
            return SignInStep::Idle;
        }
        let identity = match answer {
            Ok(identity) => identity,
            Err(error) => return self.fail(error.to_string()),
        };
        let (Some(start), Some(tokens)) = (&self.start, self.tokens.clone()) else {
            return self.fail("The sign-in lost its token; start again.".to_owned());
        };
        let key = match AccountKey::new(start.host().clone(), &identity.login) {
            Ok(key) => key,
            Err(problem) => return self.fail(problem),
        };
        let account = GitHubAccount {
            key,
            method: start.method(),
            client_id: start.client(),
            scopes: identity.scopes,
            token_expires: identity.expires,
        };
        self.enter(SignInPhase::Saving);
        SignInStep::Save {
            generation,
            account,
            tokens,
        }
    }

    /// The save answered.
    pub fn saved(
        &mut self,
        generation: u64,
        account: GitHubAccount,
        answer: Result<(), String>,
    ) -> SignInStep {
        if !self.current(generation) || self.phase != SignInPhase::Saving {
            return SignInStep::Idle;
        }
        match answer {
            Ok(()) => {
                self.tokens = None;
                self.enter(SignInPhase::SignedIn { account });
                SignInStep::Idle
            }
            Err(message) => self.fail(message),
        }
    }
}
