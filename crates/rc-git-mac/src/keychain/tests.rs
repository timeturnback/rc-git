use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rc_git_kit::platform::credentials::CachingCredentialStore;
use rc_git_kit::platform::settings::Value;

use super::*;

/// The identifying query as plain values, keyed by the framework's attribute names.
fn query_values(kind: CredentialKind) -> BTreeMap<String, Value> {
    let store = KeychainCredentialStore::new();
    let query = store.query("example.com:8443", kind, "read").unwrap();
    // SAFETY: `query` is live for the call.
    let Some(Value::Dict(map)) = (unsafe { cf::to_value(query.as_ptr()) }) else {
        panic!("the query is a dictionary");
    };
    map
}

/// The text of a framework string constant.
fn text(constant: CFTypeRef) -> String {
    // SAFETY: every constant passed here is a CF string.
    unsafe { cf::string_text(constant) }.unwrap()
}

#[test]
fn the_query_is_scoped_to_rc_git_items() {
    // SAFETY: framework constants.
    let (class, server, protocol, domain, internet, https, ssh) = unsafe {
        (
            text(kSecClass),
            text(kSecAttrServer),
            text(kSecAttrProtocol),
            text(kSecAttrSecurityDomain),
            text(kSecClassInternetPassword),
            text(kSecAttrProtocolHTTPS),
            text(kSecAttrProtocolSSH),
        )
    };
    for (kind, expected) in [
        (CredentialKind::HttpsToken, https),
        (CredentialKind::SshPassphrase, ssh),
    ] {
        let map = query_values(kind);
        assert_eq!(map.len(), 4, "{map:?}");
        assert_eq!(map[&domain], Value::String("rc-git".to_owned()));
        assert_eq!(map[&protocol], Value::String(expected));
        assert_eq!(map[&server], Value::String("example.com:8443".to_owned()));
        assert_eq!(map[&class], Value::String(internet.clone()));
    }
}

#[test]
fn labels_name_the_host_and_the_ssh_kind() {
    assert_eq!(label("h", CredentialKind::HttpsToken), "rc-git — h");
    assert_eq!(
        label("h", CredentialKind::SshPassphrase),
        "rc-git — h (ssh key)"
    );
}

/// A store whose items cannot collide with rc-git's or another run's: its own security
/// domain. Every item it may have written is deleted on drop.
struct ScratchKeychain {
    store: KeychainCredentialStore,
    host: String,
}

impl ScratchKeychain {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let unique = format!("{}-{nanos}", std::process::id());
        Self {
            store: KeychainCredentialStore {
                security_domain: format!("rc-git-test-{unique}"),
            },
            host: format!("rc-git-test-{unique}.invalid"),
        }
    }
}

impl Drop for ScratchKeychain {
    fn drop(&mut self) {
        for kind in [CredentialKind::HttpsToken, CredentialKind::SshPassphrase] {
            let _ = self.store.delete(&self.host, kind);
        }
    }
}

#[test]
fn a_token_round_trips_through_the_login_keychain() {
    let scratch = ScratchKeychain::new();
    let (store, host) = (&scratch.store, scratch.host.as_str());
    assert_eq!(
        store.credentials(host, CredentialKind::HttpsToken).unwrap(),
        None
    );

    let first = Credentials {
        username: "git".to_owned(),
        token: "tok-ừ-1".to_owned(),
    };
    store
        .store(&first, host, CredentialKind::HttpsToken)
        .unwrap();
    assert_eq!(
        store.credentials(host, CredentialKind::HttpsToken).unwrap(),
        Some(first)
    );
    assert_eq!(
        store
            .credentials(host, CredentialKind::SshPassphrase)
            .unwrap(),
        None
    );

    let second = Credentials {
        username: String::new(),
        token: "passphrase".to_owned(),
    };
    store
        .store(&second, host, CredentialKind::HttpsToken)
        .unwrap();
    assert_eq!(
        store.credentials(host, CredentialKind::HttpsToken).unwrap(),
        Some(second),
        "a store replaces the earlier item"
    );

    let rc_git = KeychainCredentialStore::new();
    assert_eq!(
        rc_git
            .credentials(host, CredentialKind::HttpsToken)
            .unwrap(),
        None,
        "another security domain's item is never read"
    );
}

#[test]
fn the_cache_in_front_of_the_keychain_redacts_its_debug() {
    let scratch = ScratchKeychain::new();
    let cache = CachingCredentialStore::new(scratch.store.clone());
    let secret = Credentials {
        username: "git".to_owned(),
        token: "do-not-print".to_owned(),
    };
    cache
        .store(&secret, &scratch.host, CredentialKind::SshPassphrase)
        .unwrap();
    let printed = format!("{cache:?}");
    assert!(!printed.contains("do-not-print"), "{printed}");
    let _ = scratch
        .store
        .delete(&scratch.host, CredentialKind::SshPassphrase);
}
