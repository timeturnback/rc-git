//! Port of `HostKeyStore.swift`: fingerprints of ssh host keys the user chose to trust, one
//! per host, and the Settings list of them. rc-git never writes `~/.ssh/known_hosts`;
//! approvals live app-side, in the settings store, since fingerprints are public values, not
//! secrets.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use crate::natural_sort;

use super::keys;
use super::settings::{SettingsError, Value};
use super::stores::SharedSettings;

/// One host the user approved, and the fingerprint they approved for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostKeyApproval {
    /// The host, as the ssh transport names it.
    pub host: String,
    /// The approved `SHA256:...` fingerprint.
    pub fingerprint: String,
}

/// Approved host-key fingerprints, one per host.
pub trait HostKeyStore: Send + Sync {
    /// The fingerprint approved for `host`.
    fn fingerprint(&self, host: &str) -> Option<String>;

    /// Approves `fingerprint` for `host`, replacing any earlier approval.
    ///
    /// # Errors
    /// [`SettingsError`] naming the approvals key when the store refused the write.
    fn store(&self, fingerprint: &str, host: &str) -> Result<(), SettingsError>;

    /// Drops the approval for `host`, so a legitimately rotated key can be approved anew.
    ///
    /// # Errors
    /// [`SettingsError`] naming the approvals key when the store refused the write.
    fn forget(&self, host: &str) -> Result<(), SettingsError>;

    /// Every approval, in no particular order; [`ApprovedHostsModel`] sorts them.
    fn approvals(&self) -> Vec<HostKeyApproval>;
}

/// A shared store is a store: the app holds one behind an `Arc` for every window, and
/// Settings lists the same one.
impl<T: HostKeyStore + ?Sized> HostKeyStore for std::sync::Arc<T> {
    fn fingerprint(&self, host: &str) -> Option<String> {
        (**self).fingerprint(host)
    }

    fn store(&self, fingerprint: &str, host: &str) -> Result<(), SettingsError> {
        (**self).store(fingerprint, host)
    }

    fn forget(&self, host: &str) -> Result<(), SettingsError> {
        (**self).forget(host)
    }

    fn approvals(&self) -> Vec<HostKeyApproval> {
        (**self).approvals()
    }
}

/// The approvals as one host-to-fingerprint map under [`keys::ACCEPTED_HOST_KEYS`]. Entries
/// are read one by one, so a value that is not a string under one host neither hides nor
/// wipes the approvals stored for the others.
pub struct SettingsHostKeyStore {
    settings: SharedSettings,
    /// Serializes read-modify-write, so two approvals at once cannot drop each other.
    writes: Mutex<()>,
}

impl SettingsHostKeyStore {
    /// The approvals kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self {
            settings,
            writes: Mutex::new(()),
        }
    }

    fn stored(&self) -> BTreeMap<String, String> {
        self.settings
            .string_map(keys::ACCEPTED_HOST_KEYS)
            .unwrap_or_default()
    }

    fn update(
        &self,
        edit: impl FnOnce(&mut BTreeMap<String, String>),
    ) -> Result<(), SettingsError> {
        let _serial = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut entries = self.stored();
        edit(&mut entries);
        self.settings
            .set_value(keys::ACCEPTED_HOST_KEYS, Some(Value::string_map(&entries)))
    }
}

impl HostKeyStore for SettingsHostKeyStore {
    fn fingerprint(&self, host: &str) -> Option<String> {
        self.stored().remove(host)
    }

    fn store(&self, fingerprint: &str, host: &str) -> Result<(), SettingsError> {
        self.update(|entries| {
            entries.insert(host.to_owned(), fingerprint.to_owned());
        })
    }

    fn forget(&self, host: &str) -> Result<(), SettingsError> {
        self.update(|entries| {
            entries.remove(host);
        })
    }

    fn approvals(&self) -> Vec<HostKeyApproval> {
        let mut approvals = Vec::new();
        for (host, fingerprint) in self.stored() {
            approvals.push(HostKeyApproval { host, fingerprint });
        }
        approvals
    }
}

/// A [`HostKeyStore`] held in memory, for model tests.
#[derive(Debug, Default)]
pub struct MemoryHostKeyStore {
    entries: Mutex<BTreeMap<String, String>>,
}

impl MemoryHostKeyStore {
    /// No approvals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HostKeyStore for MemoryHostKeyStore {
    fn fingerprint(&self, host: &str) -> Option<String> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.get(host).cloned()
    }

    fn store(&self, fingerprint: &str, host: &str) -> Result<(), SettingsError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(host.to_owned(), fingerprint.to_owned());
        Ok(())
    }

    fn forget(&self, host: &str) -> Result<(), SettingsError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.remove(host);
        Ok(())
    }

    fn approvals(&self) -> Vec<HostKeyApproval> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut approvals = Vec::new();
        for (host, fingerprint) in entries.iter() {
            approvals.push(HostKeyApproval {
                host: host.clone(),
                fingerprint: fingerprint.clone(),
            });
        }
        approvals
    }
}

/// The approved ssh hosts as Settings lists them, sorted by host, with Remove. Removing an
/// approval is how a user recovers from a legitimately rotated key: the next fetch from that
/// host meets the new key as unknown and offers the trust sheet again, rather than refusing it
/// as a changed key.
///
/// Plain state: the view that owns it wraps it in an entity and notifies when
/// [`ApprovedHostsModel::reload`] or [`ApprovedHostsModel::remove`] says the rows changed.
pub struct ApprovedHostsModel<S> {
    rows: Vec<HostKeyApproval>,
    store: S,
}

impl<S: HostKeyStore> ApprovedHostsModel<S> {
    /// The rows `store` holds now.
    pub fn new(store: S) -> Self {
        let rows = sorted(store.approvals());
        Self { rows, store }
    }

    /// The approvals, sorted by host the way Finder sorts names.
    #[must_use]
    pub fn rows(&self) -> &[HostKeyApproval] {
        &self.rows
    }

    /// Re-reads the store: a window may have approved a host since the list was last shown.
    pub fn reload(&mut self) {
        self.rows = sorted(self.store.approvals());
    }

    /// Forgets the approvals at `indexes` into [`Self::rows`]; an index past the end is
    /// ignored. Returns whether anything was removed, so the caller notifies only then.
    ///
    /// # Errors
    /// The first [`SettingsError`] a forget met. The rows are re-read either way, so they
    /// show what the store holds.
    pub fn remove(&mut self, indexes: &[usize]) -> Result<bool, SettingsError> {
        let mut hosts = Vec::new();
        for index in indexes {
            if let Some(row) = self.rows.get(*index) {
                hosts.push(row.host.clone());
            }
        }
        if hosts.is_empty() {
            return Ok(false);
        }
        let mut first_error = None;
        for host in &hosts {
            if let Err(error) = self.store.forget(host) {
                first_error.get_or_insert(error);
            }
        }
        self.reload();
        match first_error {
            Some(error) => Err(error),
            None => Ok(true),
        }
    }
}

fn sorted(mut approvals: Vec<HostKeyApproval>) -> Vec<HostKeyApproval> {
    approvals.sort_by(|a, b| natural_sort::compare(&a.host, &b.host));
    approvals
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::platform::settings::MemorySettings;
    use crate::platform::settings::SettingsStore;

    fn scratch() -> (Arc<MemorySettings>, SettingsHostKeyStore) {
        let settings = Arc::new(MemorySettings::new());
        let store = SettingsHostKeyStore::new(settings.clone());
        (settings, store)
    }

    fn approval(host: &str, fingerprint: &str) -> HostKeyApproval {
        HostKeyApproval {
            host: host.to_owned(),
            fingerprint: fingerprint.to_owned(),
        }
    }

    fn mixed_entries() -> Value {
        let mut dict = BTreeMap::new();
        dict.insert("good.test".to_owned(), Value::String("SHA256:aaa".into()));
        dict.insert("bad.test".to_owned(), Value::Int(42));
        Value::Dict(dict)
    }

    #[test]
    fn an_unknown_host_has_no_fingerprint() {
        let (_, store) = scratch();
        assert_eq!(store.fingerprint("example.test"), None);
    }

    #[test]
    fn a_stored_fingerprint_reads_back_from_a_new_store() {
        let (settings, store) = scratch();
        store.store("SHA256:aaa", "example.test").unwrap();
        let reopened = SettingsHostKeyStore::new(settings);
        assert_eq!(
            reopened.fingerprint("example.test").as_deref(),
            Some("SHA256:aaa")
        );
    }

    #[test]
    fn hosts_stay_independent() {
        let (_, store) = scratch();
        store.store("SHA256:aaa", "one.test").unwrap();
        store.store("SHA256:bbb", "two.test").unwrap();
        assert_eq!(store.fingerprint("one.test").as_deref(), Some("SHA256:aaa"));
        assert_eq!(store.fingerprint("two.test").as_deref(), Some("SHA256:bbb"));
    }

    #[test]
    fn overwriting_one_host_leaves_the_other() {
        let (_, store) = scratch();
        store.store("SHA256:aaa", "one.test").unwrap();
        store.store("SHA256:bbb", "two.test").unwrap();
        store.store("SHA256:ccc", "one.test").unwrap();
        assert_eq!(store.fingerprint("one.test").as_deref(), Some("SHA256:ccc"));
        assert_eq!(store.fingerprint("two.test").as_deref(), Some("SHA256:bbb"));
    }

    #[test]
    fn forgetting_one_host_leaves_the_other() {
        let (_, store) = scratch();
        store.store("SHA256:aaa", "one.test").unwrap();
        store.store("SHA256:bbb", "two.test").unwrap();
        store.forget("one.test").unwrap();
        assert_eq!(store.fingerprint("one.test"), None);
        assert_eq!(store.fingerprint("two.test").as_deref(), Some("SHA256:bbb"));
    }

    #[test]
    fn a_non_string_value_hides_no_other_host() {
        let (settings, store) = scratch();
        settings
            .set_value(keys::ACCEPTED_HOST_KEYS, Some(mixed_entries()))
            .unwrap();
        assert_eq!(
            store.fingerprint("good.test").as_deref(),
            Some("SHA256:aaa")
        );
        assert_eq!(store.fingerprint("bad.test"), None);
        store.store("SHA256:bbb", "new.test").unwrap();
        assert_eq!(
            store.fingerprint("good.test").as_deref(),
            Some("SHA256:aaa")
        );
        assert_eq!(store.fingerprint("new.test").as_deref(), Some("SHA256:bbb"));
    }

    #[test]
    fn approvals_list_every_string_entry() {
        let (settings, store) = scratch();
        settings
            .set_value(keys::ACCEPTED_HOST_KEYS, Some(mixed_entries()))
            .unwrap();
        assert_eq!(store.approvals(), vec![approval("good.test", "SHA256:aaa")]);
    }

    #[test]
    fn a_refused_write_names_the_key() {
        let (settings, store) = scratch();
        settings.fail_writes(true);
        let error = store.store("SHA256:aaa", "one.test").unwrap_err();
        assert_eq!(error.key, "rcgit.acceptedHostKeys");
    }

    #[test]
    fn rows_are_sorted_by_host() {
        let (_, store) = scratch();
        store.store("SHA256:bbb", "host10.test").unwrap();
        store.store("SHA256:aaa", "host2.test").unwrap();
        store.store("SHA256:ccc", "Alpha.test").unwrap();
        let model = ApprovedHostsModel::new(store);
        let hosts: Vec<&str> = model.rows().iter().map(|row| row.host.as_str()).collect();
        assert_eq!(hosts, ["Alpha.test", "host2.test", "host10.test"]);
    }

    #[test]
    fn remove_forgets_only_the_chosen_hosts() {
        let (settings, store) = scratch();
        store.store("SHA256:aaa", "one.test").unwrap();
        store.store("SHA256:bbb", "two.test").unwrap();
        store.store("SHA256:ccc", "three.test").unwrap();
        let mut model = ApprovedHostsModel::new(store);
        assert_eq!(model.remove(&[0, 2]), Ok(true));
        let check = SettingsHostKeyStore::new(settings);
        assert_eq!(check.fingerprint("one.test"), None);
        assert_eq!(check.fingerprint("two.test"), None);
        assert_eq!(
            check.fingerprint("three.test").as_deref(),
            Some("SHA256:ccc")
        );
        assert_eq!(model.rows(), [approval("three.test", "SHA256:ccc")]);
    }

    #[test]
    fn remove_ignores_an_index_past_the_end() {
        let (_, store) = scratch();
        store.store("SHA256:aaa", "one.test").unwrap();
        let mut model = ApprovedHostsModel::new(store);
        assert_eq!(model.remove(&[1, 7]), Ok(false));
        assert_eq!(model.rows(), [approval("one.test", "SHA256:aaa")]);
    }

    #[test]
    fn reload_picks_up_an_approval_made_elsewhere() {
        let (settings, store) = scratch();
        let mut model = ApprovedHostsModel::new(store);
        assert!(model.rows().is_empty());
        SettingsHostKeyStore::new(settings)
            .store("SHA256:aaa", "one.test")
            .unwrap();
        model.reload();
        assert_eq!(model.rows(), [approval("one.test", "SHA256:aaa")]);
    }

    #[test]
    fn a_failed_remove_reports_and_still_reloads() {
        let (settings, store) = scratch();
        store.store("SHA256:aaa", "one.test").unwrap();
        let mut model = ApprovedHostsModel::new(store);
        settings.fail_writes(true);
        assert!(model.remove(&[0]).is_err());
        assert_eq!(model.rows(), [approval("one.test", "SHA256:aaa")]);
    }

    #[test]
    fn the_memory_store_round_trips() {
        let store = MemoryHostKeyStore::new();
        store.store("SHA256:aaa", "one.test").unwrap();
        assert_eq!(store.approvals(), vec![approval("one.test", "SHA256:aaa")]);
        store.forget("one.test").unwrap();
        assert_eq!(store.fingerprint("one.test"), None);
    }
}
