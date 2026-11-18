//! [`CredentialStore`] on the login keychain (`CredentialStore.swift`,
//! `KeychainCredentialStore`): one `kSecClassInternetPassword` item per host and kind. The
//! host is `kSecAttrServer`, the kind picks `kSecAttrProtocol`, the username rides along as
//! `kSecAttrAccount`, and the token is the item data. Every query carries the security domain
//! `rc-git`, part of an internet password's primary key, so it never reads, overwrites or
//! deletes another program's item for the same host (git-credential-osxkeychain, Safari). The
//! attributes are exactly the Swift app's, so its saved tokens read back.
//!
//! The app is ad-hoc signed, so each rebuild is a new code identity and the keychain may ask
//! again per host; wrap this store in `CachingCredentialStore` so a launch asks once.

use std::ptr;

use rc_git_kit::guard_bridge::Credentials;
use rc_git_kit::platform::credentials::{CredentialError, CredentialKind, CredentialStore};

use crate::cf::{self, CFTypeRef, Owned};

type OsStatus = i32;
const ERR_SEC_SUCCESS: OsStatus = 0;
const ERR_SEC_ITEM_NOT_FOUND: OsStatus = -25300;
/// The status a read reports when the item exists but its data is not UTF-8 text.
const ERR_SEC_DECODE: OsStatus = -26275;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: CFTypeRef;
    static kSecClassInternetPassword: CFTypeRef;
    static kSecAttrServer: CFTypeRef;
    static kSecAttrProtocol: CFTypeRef;
    static kSecAttrProtocolHTTPS: CFTypeRef;
    static kSecAttrProtocolSSH: CFTypeRef;
    static kSecAttrSecurityDomain: CFTypeRef;
    static kSecAttrAccount: CFTypeRef;
    static kSecAttrLabel: CFTypeRef;
    static kSecValueData: CFTypeRef;
    static kSecReturnAttributes: CFTypeRef;
    static kSecReturnData: CFTypeRef;
    static kSecMatchLimit: CFTypeRef;
    static kSecMatchLimitOne: CFTypeRef;
    fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OsStatus;
    fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OsStatus;
    fn SecItemDelete(query: CFTypeRef) -> OsStatus;
}

/// The security domain on every rc-git item.
pub const SECURITY_DOMAIN: &str = "rc-git";

/// The login keychain.
#[derive(Debug, Clone)]
pub struct KeychainCredentialStore {
    security_domain: String,
}

impl Default for KeychainCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The CF strings one query borrows, kept alive while the query is built and used.
struct QueryParts {
    host: Owned,
    domain: Owned,
}

impl KeychainCredentialStore {
    /// rc-git's items in the login keychain.
    #[must_use]
    pub fn new() -> Self {
        Self {
            security_domain: SECURITY_DOMAIN.to_owned(),
        }
    }

    fn parts(&self, host: &str, op: &'static str) -> Result<QueryParts, CredentialError> {
        let failed = || CredentialError {
            op,
            host: host.to_owned(),
            status: ERR_SEC_DECODE,
        };
        Ok(QueryParts {
            host: cf::string(host).ok_or_else(failed)?,
            domain: cf::string(&self.security_domain).ok_or_else(failed)?,
        })
    }

    /// The attributes that identify rc-git's item for a host and kind, as key-value pairs
    /// borrowed from `parts` and the framework constants.
    fn identity(parts: &QueryParts, kind: CredentialKind) -> Vec<(CFTypeRef, CFTypeRef)> {
        // SAFETY: framework constants, initialized before any Rust code runs.
        unsafe {
            let protocol = match kind {
                CredentialKind::HttpsToken => kSecAttrProtocolHTTPS,
                CredentialKind::SshPassphrase => kSecAttrProtocolSSH,
            };
            vec![
                (kSecClass, kSecClassInternetPassword),
                (kSecAttrServer, parts.host.as_ptr()),
                (kSecAttrProtocol, protocol),
                (kSecAttrSecurityDomain, parts.domain.as_ptr()),
            ]
        }
    }

    /// The identifying query for `host` and `kind` as a CF dictionary.
    fn query(
        &self,
        host: &str,
        kind: CredentialKind,
        op: &'static str,
    ) -> Result<Owned, CredentialError> {
        let parts = self.parts(host, op)?;
        cf::dictionary(&Self::identity(&parts, kind)).ok_or_else(|| CredentialError {
            op,
            host: host.to_owned(),
            status: ERR_SEC_DECODE,
        })
    }

    /// A store under its own security domain, for other modules' tests.
    #[cfg(test)]
    pub(crate) fn with_security_domain(domain: &str) -> Self {
        Self {
            security_domain: domain.to_owned(),
        }
    }

    /// Deletes the item for `host` and `kind`, for other modules' tests to clean up.
    #[cfg(test)]
    pub(crate) fn delete_item(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(), CredentialError> {
        self.delete(host, kind)
    }

    /// Deletes rc-git's item for `host` and `kind`; deleting what is not there succeeds.
    fn delete(&self, host: &str, kind: CredentialKind) -> Result<(), CredentialError> {
        let query = self.query(host, kind, "write")?;
        // SAFETY: `query` is a live dictionary.
        let status = unsafe { SecItemDelete(query.as_ptr()) };
        if status == ERR_SEC_SUCCESS || status == ERR_SEC_ITEM_NOT_FOUND {
            return Ok(());
        }
        Err(CredentialError {
            op: "write",
            host: host.to_owned(),
            status,
        })
    }
}

fn label(host: &str, kind: CredentialKind) -> String {
    match kind {
        CredentialKind::HttpsToken => format!("rc-git — {host}"),
        CredentialKind::SshPassphrase => format!("rc-git — {host} (ssh key)"),
    }
}

impl CredentialStore for KeychainCredentialStore {
    fn credentials(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<Option<Credentials>, CredentialError> {
        let failed = |status| CredentialError {
            op: "read",
            host: host.to_owned(),
            status,
        };
        let parts = self.parts(host, "read")?;
        let mut pairs = Self::identity(&parts, kind);
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
        // SAFETY: `item` is a live dictionary; the values are borrowed from it while it lives.
        let (data, account) = unsafe {
            (
                cf::dictionary_get(item.as_ptr(), kSecValueData),
                cf::dictionary_get(item.as_ptr(), kSecAttrAccount),
            )
        };
        if data.is_null() {
            return Err(failed(status));
        }
        // SAFETY: the data value of a returned item is CF data.
        let token =
            String::from_utf8(unsafe { cf::data_bytes(data) }).map_err(|_| failed(status))?;
        let username = if account.is_null() {
            String::new()
        } else {
            // SAFETY: the account attribute is a CF string.
            unsafe { cf::string_text(account) }.unwrap_or_default()
        };
        Ok(Some(Credentials { username, token }))
    }

    fn store(
        &self,
        credentials: &Credentials,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(), CredentialError> {
        let failed = |status| CredentialError {
            op: "write",
            host: host.to_owned(),
            status,
        };
        self.delete(host, kind)?;
        let parts = self.parts(host, "write")?;
        let account = cf::string(&credentials.username).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        let data = cf::data(credentials.token.as_bytes()).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        let label = cf::string(&label(host, kind)).ok_or_else(|| failed(ERR_SEC_DECODE))?;
        let mut pairs = Self::identity(&parts, kind);
        // SAFETY: framework constants.
        unsafe {
            pairs.push((kSecAttrAccount, account.as_ptr()));
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
}

#[cfg(test)]
mod tests;
