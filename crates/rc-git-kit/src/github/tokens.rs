//! A working token for an account, renewing a GitHub App's expired one. The store is the
//! durable record and two writers share it (the app and the CLI), so:
//!
//! - every call reads the store, and a token the store no longer holds is never served: a
//!   sign-out by either process stops the other at its next call;
//! - a refresh token is single-use, so refreshes of one account are serialized in this
//!   process: one runs while the others wait, then read the renewed token from the store;
//! - a renewal goes back into the existing item only ([`ApiTokenStore::update_tokens`]); when
//!   the item is gone, the account was signed out meanwhile and the renewal is dropped;
//! - a refused refresh reads the store once more, and only when it still holds the refresh
//!   token that was refused is the account signed out. A failure to reach the host is never a
//!   sign-out: it is reported as it is.
//!
//! The keeper remembers one thing between calls: a renewal the store refused to save, served
//! only while the store still holds the refresh token that renewal spent, since that token
//! cannot buy another.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rc_git_guard::actions::github::{ApiToken, GitHubError, RefreshToken, TokenSet};

use super::accounts::GitHubAccount;
use crate::platform::api_tokens::{AccountKey, ApiTokenStore, TokenUpdate};
use crate::platform::credentials::CredentialError;

/// How long before its expiry an access token counts as expired, so a call that starts with
/// it does not end after it.
pub const EXPIRY_MARGIN_SECS: u64 = 5 * 60;

/// Why no token is available.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// No token is stored (a sign-out by either process included), or the stored one expired
    /// and cannot be renewed, or the host refused its refresh token: the user must sign in
    /// again.
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
    /// Set when a refresh renewed the token but the store refused the new pair: this process
    /// keeps the renewal while the store still holds the refresh token it spent; the next
    /// launch is signed out.
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

/// A renewal the store refused to save: the refresh token it spent and what it bought.
struct Unsaved {
    spent: RefreshToken,
    renewed: TokenSet,
}

/// A slot's lock held: serializes one account's refreshes and guards its unsaved renewal.
type Held<'a> = MutexGuard<'a, Option<Unsaved>>;

/// Hands out each account's working token, renewing it as described in the module docs.
pub struct TokenKeeper<S> {
    store: S,
    /// One slot per account: its lock serializes refreshes, its value is the renewal the store
    /// refused to save, when there is one.
    slots: Mutex<HashMap<AccountKey, Arc<Mutex<Option<Unsaved>>>>>,
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

    fn slot(&self, key: &AccountKey) -> Arc<Mutex<Option<Unsaved>>> {
        let mut slots = lock(&self.slots);
        Arc::clone(slots.entry(key.clone()).or_default())
    }

    /// `account`'s token at `now` (seconds since the Unix epoch), read from the store on every
    /// call and renewed through `refresh` when it expired. `refresh` runs with this account's
    /// slot held, so no two run at once.
    ///
    /// # Errors
    /// [`TokenError::SignedOut`] when nothing usable is stored, the host refused the stored
    /// refresh token, or the account was signed out while its token renewed;
    /// [`TokenError::Store`] when the store cannot be read; [`TokenError::GitHub`] when the
    /// refresh failed for another reason.
    pub fn token(
        &self,
        account: &GitHubAccount,
        now: u64,
        refresh: impl FnOnce(&RefreshToken) -> Result<TokenSet, GitHubError>,
    ) -> Result<FreshToken, TokenError> {
        let key = &account.key;
        let slot = self.slot(key);
        let mut held = lock(&slot);
        let Some(stored) = self.store.tokens(key)? else {
            *held = None;
            return Err(signed_out(key, "no token is stored"));
        };
        if is_fresh(&stored, now) {
            *held = None;
            return Ok(fresh(stored.access));
        }
        let spent_here = held.as_ref().filter(|unsaved| {
            stored.refresh.as_ref() == Some(&unsaved.spent) && is_fresh(&unsaved.renewed, now)
        });
        if let Some(unsaved) = spent_here {
            return Ok(fresh(unsaved.renewed.access.clone()));
        }
        *held = None;
        let Some(used) = stored.refresh.clone().filter(|_| can_refresh(&stored, now)) else {
            return Err(signed_out(key, "the token expired and cannot be renewed"));
        };
        match refresh(&used) {
            Ok(renewed) => self.keep(key, &mut held, used, renewed),
            Err(GitHubError::RefreshRejected { .. }) => self.renewed_elsewhere(key, &used, now),
            Err(error) => Err(TokenError::GitHub(error)),
        }
    }

    /// Puts a renewal back into `key`'s item. A store that refuses it leaves the renewal held
    /// for this session; an item that is gone means the account was signed out meanwhile.
    fn keep(
        &self,
        key: &AccountKey,
        held: &mut Held<'_>,
        spent: RefreshToken,
        renewed: TokenSet,
    ) -> Result<FreshToken, TokenError> {
        match self.store.update_tokens(key, &renewed) {
            Ok(TokenUpdate::Replaced) => Ok(fresh(renewed.access)),
            Ok(TokenUpdate::NoItem) => Err(signed_out(
                key,
                "the account was signed out while its token renewed",
            )),
            Err(unsaved) => {
                let token = renewed.access.clone();
                **held = Some(Unsaved { spent, renewed });
                Ok(FreshToken {
                    token,
                    unsaved: Some(unsaved),
                })
            }
        }
    }

    /// After the host refused `used`: the token another writer renewed with it, if the store
    /// now holds one, else a sign-out.
    fn renewed_elsewhere(
        &self,
        key: &AccountKey,
        used: &RefreshToken,
        now: u64,
    ) -> Result<FreshToken, TokenError> {
        let again = self.store.tokens(key)?;
        let renewed = again
            .filter(|tokens| tokens.refresh.as_ref() != Some(used))
            .filter(|tokens| is_fresh(tokens, now));
        match renewed {
            Some(tokens) => Ok(fresh(tokens.access)),
            None => Err(signed_out(key, "the host refused the refresh token")),
        }
    }
}

fn signed_out(key: &AccountKey, reason: &'static str) -> TokenError {
    TokenError::SignedOut {
        account: key.to_string(),
        reason,
    }
}

fn fresh(token: ApiToken) -> FreshToken {
    FreshToken {
        token,
        unsaved: None,
    }
}
