//! [`ApiTokenStore`] on the login keychain: one `kSecClassGenericPassword` item per GitHub host
//! and account, service `rc-git`, account `github-api/<host>/<login>`, holding the encoded
//! token set (access token, refresh token, expiries) as its data.
//!
//! The class keeps these items apart from the HTTPS push tokens and ssh passphrases of
//! [`crate::keychain`], which are internet passwords queried by server and protocol alone: an
//! internet-password item of rc-git's for the same host would match those queries, and the
//! push store's delete-then-add would erase it. A generic password matches none of them, and
//! their queries match none of these. Every query here names the service and the full account,
//! so it reads, replaces or deletes exactly one item.
//!
//! A sign-in deletes whatever item holds its account and adds a fresh one, as the push store
//! does: an item another program planted under rc-git's service and account, with an access
//! list of its own choosing, would otherwise receive the user's token and keep that list. An
//! item that reappears between the delete and the add makes the add fail; it is never
//! overwritten. A refresh replaces the data of the item the sign-in created in place
//! (`SecItemUpdate`), so a second writer (the CLI) never sees the account without a token
//! mid-refresh, and never adds one: a refresh that finds no item stores nothing.

use std::ptr;

use rc_git_kit::github::TokenSet;
use rc_git_kit::platform::api_tokens::{
    AccountKey, ApiTokenStore, TokenUpdate, decode_tokens, encode_tokens,
};
use rc_git_kit::platform::credentials::CredentialError;

use crate::cf::{self, CFTypeRef, Owned};

type OsStatus = i32;
const ERR_SEC_SUCCESS: OsStatus = 0;
const ERR_SEC_ITEM_NOT_FOUND: OsStatus = -25300;
/// The status a read reports when the item's data is not a token set rc-git wrote.
const ERR_SEC_DECODE: OsStatus = -26275;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: CFTypeRef;
    static kSecClassGenericPassword: CFTypeRef;
    static kSecAttrService: CFTypeRef;
    static kSecAttrAccount: CFTypeRef;
    static kSecAttrLabel: CFTypeRef;
    static kSecValueData: CFTypeRef;
    static kSecReturnAttributes: CFTypeRef;
    static kSecReturnData: CFTypeRef;
    static kSecMatchLimit: CFTypeRef;
    static kSecMatchLimitOne: CFTypeRef;
    fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OsStatus;
    fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OsStatus;
    fn SecItemUpdate(query: CFTypeRef, attributes: CFTypeRef) -> OsStatus;
    fn SecItemDelete(query: CFTypeRef) -> OsStatus;
}

/// The service every GitHub API token item carries.
pub const SERVICE: &str = "rc-git";

/// What every item's account starts with, ahead of `<host>/<login>`.
const ACCOUNT_PREFIX: &str = "github-api/";

/// The GitHub API tokens in the login keychain.
#[derive(Debug, Clone)]
pub struct KeychainApiTokenStore {
    service: String,
}

impl Default for KeychainApiTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The CF strings one query borrows, kept alive while the query is built and used.
struct QueryParts {
    service: Owned,
    account: Owned,
}

impl KeychainApiTokenStore {
    /// rc-git's items.
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: SERVICE.to_owned(),
        }
    }

    /// Items under another service: `rc-git-` and a suffix of letters, digits and dashes, so a
    /// test or a scripted run keeps its tokens apart from rc-git's own and never reaches
    /// another program's.
    ///
    /// # Errors
    /// The refusal, naming the service, for any other name.
    pub fn with_service(service: &str) -> Result<Self, String> {
        let suffix = service.strip_prefix("rc-git-").unwrap_or_default();
        let plain = suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if suffix.is_empty() || suffix.len() > 100 || !plain {
            return Err(format!(
                "keychain service '{service}' must be 'rc-git-' followed by letters, digits or \
                 dashes"
            ));
        }
        Ok(Self {
            service: service.to_owned(),
        })
    }

    fn failed(op: &'static str, key: &AccountKey, status: OsStatus) -> CredentialError {
        CredentialError {
            op,
            host: key.to_string(),
            status,
        }
    }

    fn parts(&self, key: &AccountKey, op: &'static str) -> Result<QueryParts, CredentialError> {
        let account = format!("{ACCOUNT_PREFIX}{}", key.id());
        Ok(QueryParts {
            service: cf::string(&self.service)
                .ok_or_else(|| Self::failed(op, key, ERR_SEC_DECODE))?,
            account: cf::string(&account).ok_or_else(|| Self::failed(op, key, ERR_SEC_DECODE))?,
        })
    }

    /// The attributes naming exactly one item, borrowed from `parts` and the constants.
    fn identity(parts: &QueryParts) -> Vec<(CFTypeRef, CFTypeRef)> {
        // SAFETY: framework constants, initialized before any Rust code runs.
        unsafe {
            vec![
                (kSecClass, kSecClassGenericPassword),
                (kSecAttrService, parts.service.as_ptr()),
                (kSecAttrAccount, parts.account.as_ptr()),
            ]
        }
    }

    /// The identifying query for `key`, as a CF dictionary.
    fn query(&self, key: &AccountKey, op: &'static str) -> Result<Owned, CredentialError> {
        let parts = self.parts(key, op)?;
        cf::dictionary(&Self::identity(&parts)).ok_or_else(|| Self::failed(op, key, ERR_SEC_DECODE))
    }
}

fn label(key: &AccountKey) -> String {
    format!("rc-git — GitHub API token ({key})")
}

impl ApiTokenStore for KeychainApiTokenStore {
    fn tokens(&self, key: &AccountKey) -> Result<Option<TokenSet>, CredentialError> {
        let failed = |status| Self::failed("read", key, status);
        let parts = self.parts(key, "read")?;
        let mut pairs = Self::identity(&parts);
        // SAFETY: framework constants.
        unsafe {
            pairs.push((kSecReturnAttributes, cf::boolean(true)));
            pairs.push((kSecReturnData, cf::boolean(true)));
            pairs.push((kSecMatchLimit, kSecMatchLimitOne));
        }
        let query = cf::dictionary(&pairs).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        let mut result: CFTypeRef = ptr::null();
        // SAFETY: `query` is live; on success `result` receives an owned dictionary.
        let status = unsafe { SecItemCopyMatching(query.as_ptr(), &raw mut result) };
        // SAFETY: `result` is null or the owned reference the call returned.
        let item = unsafe { Owned::from_create(result) };
        if status == ERR_SEC_ITEM_NOT_FOUND {
            return Ok(None);
        }
        let (ERR_SEC_SUCCESS, Some(item)) = (status, item) else {
            return Err(failed(status));
        };
        // SAFETY: `item` is a live dictionary; the value is borrowed from it while it lives.
        let data = unsafe { cf::dictionary_get(item.as_ptr(), kSecValueData) };
        if data.is_null() {
            return Err(failed(ERR_SEC_DECODE));
        }
        // SAFETY: the data value of a returned item is CF data.
        let bytes = unsafe { cf::data_bytes(data) };
        decode_tokens(&bytes)
            .map(Some)
            .ok_or_else(|| failed(ERR_SEC_DECODE))
    }

    fn save_new_tokens(&self, key: &AccountKey, tokens: &TokenSet) -> Result<(), CredentialError> {
        let failed = |status| Self::failed("write", key, status);
        self.delete(key, "write")?;
        let parts = self.parts(key, "write")?;
        let data = cf::data(&encode_tokens(tokens)).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        let label = cf::string(&label(key)).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        let mut pairs = Self::identity(&parts);
        // SAFETY: framework constants.
        unsafe {
            pairs.push((kSecValueData, data.as_ptr()));
            pairs.push((kSecAttrLabel, label.as_ptr()));
        }
        let attributes = cf::dictionary(&pairs).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        // SAFETY: `attributes` is live; a null result pointer asks for nothing back.
        let status = unsafe { SecItemAdd(attributes.as_ptr(), ptr::null_mut()) };
        if status != ERR_SEC_SUCCESS {
            return Err(failed(status));
        }
        Ok(())
    }

    fn update_tokens(
        &self,
        key: &AccountKey,
        tokens: &TokenSet,
    ) -> Result<TokenUpdate, CredentialError> {
        let failed = |status| Self::failed("write", key, status);
        let query = self.query(key, "write")?;
        let data = cf::data(&encode_tokens(tokens)).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        // SAFETY: framework constant.
        let changes = unsafe { [(kSecValueData, data.as_ptr())] };
        let update = cf::dictionary(&changes).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        // SAFETY: both dictionaries are live for the call.
        let status = unsafe { SecItemUpdate(query.as_ptr(), update.as_ptr()) };
        match status {
            ERR_SEC_SUCCESS => Ok(TokenUpdate::Replaced),
            ERR_SEC_ITEM_NOT_FOUND => Ok(TokenUpdate::NoItem),
            other => Err(failed(other)),
        }
    }

    fn delete_tokens(&self, key: &AccountKey) -> Result<(), CredentialError> {
        self.delete(key, "delete")
    }
}

impl KeychainApiTokenStore {
    /// Deletes `key`'s item, reporting a failure as `op`; deleting what is not there succeeds.
    fn delete(&self, key: &AccountKey, op: &'static str) -> Result<(), CredentialError> {
        let query = self.query(key, op)?;
        // SAFETY: `query` is a live dictionary.
        let status = unsafe { SecItemDelete(query.as_ptr()) };
        if status == ERR_SEC_SUCCESS || status == ERR_SEC_ITEM_NOT_FOUND {
            return Ok(());
        }
        Err(Self::failed(op, key, status))
    }
}

#[cfg(test)]
mod tests;
