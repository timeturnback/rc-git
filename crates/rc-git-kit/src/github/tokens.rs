//! A working token for an account, renewing a GitHub App's expired one. A refresh token is
//! single-use, and two writers share the keychain item (the app and the CLI), so:
//!
//! - refreshes of one account are serialized in this process: one runs while the others wait,
//!   then find the renewed token in the store and use it;
//! - before refreshing, the store is read again: another process may have renewed already;
//! - a refused refresh reads the store once more, and only when it still holds the refresh
//!   token that was refused is the account signed out. A failure to reach the host is never a
//!   sign-out: it is reported as it is.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rc_git_guard::actions::github::{ApiToken, GitHubError, RefreshToken, TokenSet};

use super::accounts::GitHubAccount;
use crate::platform::api_tokens::{AccountKey, ApiTokenStore};
use crate::platform::credentials::CredentialError;

/// How long before its expiry an access token counts as expired, so a call that starts with
/// it does not end after it.
pub const EXPIRY_MARGIN_SECS: u64 = 5 * 60;

/// Why no token is available.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// No token is stored, or the stored one expired and cannot be renewed, or the host
    /// refused its refresh token: the user must sign in again.
    #[error("{account}: signed out ({reason}); sign in again in Settings")]
    SignedOut {
        /// The account.
        account: String,
        /// What ended the session.
        reason: &'static str,
    },
    /// The store could not be read.
    #[error(transparent)]
    Store(#[from] CredentialError),
    /// The refresh could not reach the host or failed some other way; the stored tokens are
    /// as they were.
    #[error(transparent)]
    GitHub(#[from] GitHubError),
}

/// A token ready to send, and whether renewing it failed to save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshToken {
    /// The access token.
    pub token: ApiToken,
    /// Set when a refresh renewed the token but the store refused the new pair: this session
    /// keeps working, the next launch is signed out.
    pub unsaved: Option<CredentialError>,
}

/// Whether `tokens`' access token is still good at `now`.
#[must_use]
pub fn is_fresh(tokens: &TokenSet, now: u64) -> bool {
    tokens
        .access_expires_at
        .is_none_or(|at| at > now.saturating_add(EXPIRY_MARGIN_SECS))
}

/// Whether `tokens` hold a refresh token still good at `now`.
fn can_refresh(tokens: &TokenSet, now: u64) -> bool {
    tokens.refresh.is_some() && tokens.refresh_expires_at.is_none_or(|at| at > now)
}

/// Hands out each account's working token, renewing it as described in the module docs.
pub struct TokenKeeper<S> {
    store: S,
    /// One slot per account: its lock serializes refreshes, its value is the last pair read.
    slots: Mutex<HashMap<AccountKey, Arc<Mutex<Option<TokenSet>>>>>,
}

impl<S> std::fmt::Debug for TokenKeeper<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenKeeper")
            .field("secrets", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<S: ApiTokenStore> TokenKeeper<S> {
    /// A keeper over `store`.
    pub fn new(store: S) -> Self {
        Self {
            store,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The store.
    pub fn store(&self) -> &S {
        &self.store
    }

    fn slot(&self, key: &AccountKey) -> Arc<Mutex<Option<TokenSet>>> {
        let mut slots = lock(&self.slots);
        Arc::clone(slots.entry(key.clone()).or_default())
    }

    /// Forgets what was read for `key`, after a sign-in or sign-out replaced it.
    pub fn forget(&self, key: &AccountKey) {
        lock(&self.slots).remove(key);
    }

    /// `account`'s token at `now` (seconds since the Unix epoch), renewed through `refresh`
    /// when it expired. `refresh` runs with this account's slot held, so no two run at once.
    ///
    /// # Errors
    /// [`TokenError::SignedOut`] when nothing usable is stored or the host refused the stored
    /// refresh token, [`TokenError::Store`] when the store cannot be read,
    /// [`TokenError::GitHub`] when the refresh failed for another reason.
    pub fn token(
        &self,
        account: &GitHubAccount,
        now: u64,
        refresh: impl FnOnce(&RefreshToken) -> Result<TokenSet, GitHubError>,
    ) -> Result<FreshToken, TokenError> {
        let key = &account.key;
        let slot = self.slot(key);
        let mut held = lock(&slot);
        if let Some(tokens) = held.as_ref().filter(|tokens| is_fresh(tokens, now)) {
            return Ok(fresh(tokens.access.clone()));
        }
        let signed_out = |reason| TokenError::SignedOut {
            account: key.to_string(),
            reason,
        };
        let Some(stored) = self.store.tokens(key)? else {
            *held = None;
            return Err(signed_out("no token is stored"));
        };
        if is_fresh(&stored, now) {
            let token = stored.access.clone();
            *held = Some(stored);
            return Ok(fresh(token));
        }
        let Some(used) = stored.refresh.clone().filter(|_| can_refresh(&stored, now)) else {
            *held = Some(stored);
            return Err(signed_out("the token expired and cannot be renewed"));
        };
        match refresh(&used) {
            Ok(renewed) => {
                let unsaved = self.store.store_tokens(key, &renewed).err();
                let token = renewed.access.clone();
                *held = Some(renewed);
                Ok(FreshToken { token, unsaved })
            }
            Err(GitHubError::RefreshRejected { .. }) => {
                let again = self.store.tokens(key)?;
                let renewed_elsewhere = again
                    .filter(|tokens| tokens.refresh.as_ref() != Some(&used))
                    .filter(|tokens| is_fresh(tokens, now));
                let Some(tokens) = renewed_elsewhere else {
                    *held = None;
                    return Err(signed_out("the host refused the refresh token"));
                };
                let token = tokens.access.clone();
                *held = Some(tokens);
                Ok(fresh(token))
            }
            Err(error) => Err(TokenError::GitHub(error)),
        }
    }
}

fn fresh(token: ApiToken) -> FreshToken {
    FreshToken {
        token,
        unsaved: None,
    }
}
