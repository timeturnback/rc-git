//! Port of `CredentialStore.swift`'s protocol and cache: one secret per host and kind. The
//! login-keychain store lives in `rc-git-mac`; tests and models use [`MemoryCredentialStore`].

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::guard_bridge::Credentials;

/// What a stored secret is for, so one host can hold both an HTTPS token and an ssh key
/// passphrase without colliding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialKind {
    /// Username and token for HTTPS.
    HttpsToken,
    /// An ssh key passphrase, username empty.
    SshPassphrase,
}

/// A credential read or write that failed: the operation, the host, and the store's status.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("keychain {op} failed for '{host}' (status {status})")]
pub struct CredentialError {
    /// `"read"` or `"write"`.
    pub op: &'static str,
    /// The host the secret is for.
    pub host: String,
    /// The store's status code (an `OSStatus` for the keychain).
    pub status: i32,
}

/// Stores one secret per host and kind: username and token for HTTPS, the key passphrase
/// (username empty) for ssh.
pub trait CredentialStore: Send + Sync {
    /// The secret saved for `host` and `kind`, or `None` when there is none.
    ///
    /// # Errors
    /// [`CredentialError`] with op `"read"` naming the host when the store could not answer.
    fn credentials(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<Option<Credentials>, CredentialError>;

    /// Saves `credentials` for `host` and `kind`, replacing what was there.
    ///
    /// # Errors
    /// [`CredentialError`] with op `"write"` naming the host when the store refused it.
    fn store(
        &self,
        credentials: &Credentials,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(), CredentialError>;
}

type Key = (String, CredentialKind);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Remembers what another store answered, so one launch asks the keychain for each secret
/// once: under an ad-hoc signature every keychain read can raise an access dialog, and
/// "Allow" covers a single read. Only a found secret is kept; a miss and a failure are asked
/// again next time. Writes go through to the wrapped store first.
///
/// The wrapped store is called while the lock is held, so concurrent misses for one secret
/// share a single read.
pub struct CachingCredentialStore<S> {
    inner: S,
    cache: Mutex<HashMap<Key, Credentials>>,
}

impl<S: CredentialStore> CachingCredentialStore<S> {
    /// A cache in front of `inner`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The wrapped store.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S> fmt::Debug for CachingCredentialStore<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachingCredentialStore")
            .field("cached", &lock(&self.cache).len())
            .field("secrets", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl<S: CredentialStore> CredentialStore for CachingCredentialStore<S> {
    fn credentials(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<Option<Credentials>, CredentialError> {
        let mut cache = lock(&self.cache);
        let key = (host.to_owned(), kind);
        if let Some(hit) = cache.get(&key) {
            return Ok(Some(hit.clone()));
        }
        let found = self.inner.credentials(host, kind)?;
        if let Some(found) = &found {
            cache.insert(key, found.clone());
        }
        Ok(found)
    }

    fn store(
        &self,
        credentials: &Credentials,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(), CredentialError> {
        let mut cache = lock(&self.cache);
        self.inner.store(credentials, host, kind)?;
        cache.insert((host.to_owned(), kind), credentials.clone());
        Ok(())
    }
}

/// A [`CredentialStore`] held in memory. Counts every call, and can be told to fail the next
/// reads or every write, so a model test can assert how often the keychain would be asked.
#[derive(Default)]
pub struct MemoryCredentialStore {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    items: HashMap<Key, Credentials>,
    reads: usize,
    writes: usize,
    failing_reads: usize,
    failing_writes: bool,
}

impl MemoryCredentialStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A store already holding `credentials` for `host` and `kind`.
    #[must_use]
    pub fn with(credentials: Credentials, host: &str, kind: CredentialKind) -> Self {
        let store = Self::new();
        lock(&store.state)
            .items
            .insert((host.to_owned(), kind), credentials);
        store
    }

    /// How many reads reached this store.
    #[must_use]
    pub fn read_count(&self) -> usize {
        lock(&self.state).reads
    }

    /// How many writes reached this store.
    #[must_use]
    pub fn write_count(&self) -> usize {
        lock(&self.state).writes
    }

    /// The next `count` reads fail with status -25293 (`errSecAuthFailed`).
    pub fn fail_next_reads(&self, count: usize) {
        lock(&self.state).failing_reads = count;
    }

    /// Every later write fails (or succeeds again).
    pub fn fail_writes(&self, fail: bool) {
        lock(&self.state).failing_writes = fail;
    }
}

impl fmt::Debug for MemoryCredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("MemoryCredentialStore")
            .field("items", &state.items.len())
            .field("secrets", &"<redacted>")
            .field("reads", &state.reads)
            .field("writes", &state.writes)
            .finish_non_exhaustive()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn credentials(
        &self,
        host: &str,
        kind: CredentialKind,
    ) -> Result<Option<Credentials>, CredentialError> {
        let mut state = lock(&self.state);
        state.reads += 1;
        if state.failing_reads > 0 {
            state.failing_reads -= 1;
            return Err(CredentialError {
                op: "read",
                host: host.to_owned(),
                status: -25293,
            });
        }
        Ok(state.items.get(&(host.to_owned(), kind)).cloned())
    }

    fn store(
        &self,
        credentials: &Credentials,
        host: &str,
        kind: CredentialKind,
    ) -> Result<(), CredentialError> {
        let mut state = lock(&self.state);
        state.writes += 1;
        if state.failing_writes {
            return Err(CredentialError {
                op: "write",
                host: host.to_owned(),
                status: -25293,
            });
        }
        state
            .items
            .insert((host.to_owned(), kind), credentials.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved() -> Credentials {
        Credentials {
            username: "user".to_owned(),
            token: "saved-secret-token".to_owned(),
        }
    }

    fn holding_saved() -> MemoryCredentialStore {
        let store = MemoryCredentialStore::with(saved(), "example.com", CredentialKind::HttpsToken);
        lock(&store.state).items.insert(
            ("example.com".to_owned(), CredentialKind::SshPassphrase),
            saved(),
        );
        store
    }

    #[test]
    fn two_reads_cost_one_inner_read() {
        let cache = CachingCredentialStore::new(holding_saved());
        let first = cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap();
        let second = cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap();
        assert_eq!(first.unwrap().token, "saved-secret-token");
        assert_eq!(second.unwrap().token, "saved-secret-token");
        assert_eq!(cache.inner().read_count(), 1);
    }

    #[test]
    fn a_failing_read_is_not_cached() {
        let inner = holding_saved();
        inner.fail_next_reads(1);
        let cache = CachingCredentialStore::new(inner);
        let error = cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "keychain read failed for 'example.com' (status -25293)"
        );
        let retried = cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap();
        assert_eq!(retried.unwrap().token, "saved-secret-token");
        assert_eq!(cache.inner().read_count(), 2);
    }

    #[test]
    fn a_miss_is_asked_again() {
        let cache = CachingCredentialStore::new(MemoryCredentialStore::new());
        assert_eq!(
            cache
                .credentials("example.com", CredentialKind::HttpsToken)
                .unwrap(),
            None
        );
        assert_eq!(
            cache
                .credentials("example.com", CredentialKind::HttpsToken)
                .unwrap(),
            None
        );
        assert_eq!(cache.inner().read_count(), 2);
    }

    #[test]
    fn a_stored_secret_reads_back_without_an_inner_read() {
        let cache = CachingCredentialStore::new(MemoryCredentialStore::new());
        cache
            .store(&saved(), "example.com", CredentialKind::HttpsToken)
            .unwrap();
        let read = cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap();
        assert_eq!(read.unwrap().token, "saved-secret-token");
        assert_eq!(cache.inner().write_count(), 1);
        assert_eq!(cache.inner().read_count(), 0);
    }

    #[test]
    fn a_failed_write_caches_nothing() {
        let inner = MemoryCredentialStore::new();
        inner.fail_writes(true);
        let cache = CachingCredentialStore::new(inner);
        let error = cache
            .store(&saved(), "example.com", CredentialKind::HttpsToken)
            .unwrap_err();
        assert_eq!(error.op, "write");
        assert_eq!(
            cache
                .credentials("example.com", CredentialKind::HttpsToken)
                .unwrap(),
            None
        );
        assert_eq!(cache.inner().read_count(), 1);
    }

    #[test]
    fn kinds_are_cached_apart() {
        let cache = CachingCredentialStore::new(holding_saved());
        cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap();
        cache
            .credentials("example.com", CredentialKind::SshPassphrase)
            .unwrap();
        assert_eq!(cache.inner().read_count(), 2);
    }

    #[test]
    fn debug_never_prints_a_secret() {
        let cache = CachingCredentialStore::new(holding_saved());
        cache
            .credentials("example.com", CredentialKind::HttpsToken)
            .unwrap();
        let printed = format!("{cache:?} {:?}", cache.inner());
        assert!(!printed.contains("saved-secret-token"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
    }
}
