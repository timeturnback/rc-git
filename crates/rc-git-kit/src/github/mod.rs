//! GitHub accounts for the pull request features: the accounts and which repository uses
//! which ([`accounts`]), the sign-in state machine ([`sign_in`]), the token keeper that renews
//! a GitHub App token ([`tokens`]), and the words ([`copy`]). The network calls are
//! `rc_git_guard::actions::github`'s, re-exported here so the app needs no guard dependency;
//! the tokens are kept by a [`crate::platform::api_tokens::ApiTokenStore`].

pub mod accounts;
pub mod copy;
pub mod sign_in;
pub mod tokens;

pub use rc_git_guard::actions::github::{
    ApiToken, ClientId, DeviceFlow, DeviceMethod, DevicePoll, GitHubError, GitHubHost,
    RefreshToken, TokenIdentity, TokenSet, refresh_user_token, start_device_flow, unix_now,
    validate_token,
};

use accounts::{GitHubAccount, GitHubAccounts};

use crate::platform::api_tokens::{AccountKey, ApiTokenStore};

/// Saves a finished sign-in: the tokens first, in a fresh keychain item
/// ([`ApiTokenStore::save_new_tokens`]), then the account record, so a record never names an
/// account whose token is missing. A record left behind by a failed second write is harmless:
/// its token reads back and a retry replaces both.
///
/// # Errors
/// The keychain's or the settings' failure, as a sentence.
pub fn save_sign_in(
    store: &dyn ApiTokenStore,
    accounts: &GitHubAccounts,
    account: &GitHubAccount,
    tokens: &TokenSet,
) -> Result<(), String> {
    store
        .save_new_tokens(&account.key, tokens)
        .map_err(|error| error.to_string())?;
    accounts.save(account).map_err(|error| error.to_string())
}

/// Signs `key` out: forgets its record and its repository choices, then deletes its token.
/// The token goes last, so a failed delete leaves a token with no record, which nothing reads.
///
/// # Errors
/// The settings' or the keychain's failure, as a sentence.
pub fn sign_out(
    store: &dyn ApiTokenStore,
    accounts: &GitHubAccounts,
    key: &AccountKey,
) -> Result<(), String> {
    accounts.remove(key).map_err(|error| error.to_string())?;
    store.delete_tokens(key).map_err(|error| error.to_string())
}
