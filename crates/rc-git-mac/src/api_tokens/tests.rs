use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rc_git_kit::github::{ApiToken, GitHubHost, RefreshToken};
use rc_git_kit::guard_bridge::Credentials;
use rc_git_kit::platform::credentials::{CredentialKind, CredentialStore};
use rc_git_kit::platform::settings::Value;

use super::*;
use crate::keychain::KeychainCredentialStore;

/// A name no other run, no other test of this run, and no real item uses. The clock alone is
/// not enough: two tests starting in the same microsecond read the same time.
fn unique() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let serial = NEXT.fetch_add(1, Ordering::SeqCst);
    format!("{}-{nanos}-{serial}", std::process::id())
}

/// A token store under its own service and an account on a `.invalid` host, so nothing it
/// writes can be one of rc-git's real items. Its item is deleted on drop.
struct Scratch {
    store: KeychainApiTokenStore,
    key: AccountKey,
}

impl Scratch {
    fn new() -> Self {
        let unique = unique();
        let host = GitHubHost::parse(&format!("rc-git-test-{unique}.invalid")).unwrap();
        Self {
            store: KeychainApiTokenStore::with_service(&format!("rc-git-test-{unique}")).unwrap(),
            key: AccountKey::new(host, "octocat").unwrap(),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.store.delete_tokens(&self.key);
    }
}

fn app_tokens(access: &str, refresh: &str) -> TokenSet {
    TokenSet {
        access: ApiToken::new(access.to_owned()).unwrap(),
        access_expires_at: Some(1_800_000_000),
        refresh: Some(RefreshToken::new(refresh.to_owned()).unwrap()),
        refresh_expires_at: Some(1_900_000_000),
    }
}

#[test]
fn the_query_names_one_generic_password_of_the_rc_git_service() {
    let store = KeychainApiTokenStore::new();
    let key = AccountKey::new(GitHubHost::parse("ghe.example.com:8443").unwrap(), "oc").unwrap();
    let query = store.query(&key, "read").unwrap();
    // SAFETY: `query` is live for the call.
    let Some(Value::Dict(map)) = (unsafe { cf::to_value(query.as_ptr()) }) else {
        panic!("the query is a dictionary");
    };
    // SAFETY: framework constants, each a CF string.
    let (class, service, account, generic) = unsafe {
        (
            cf::string_text(kSecClass).unwrap(),
            cf::string_text(kSecAttrService).unwrap(),
            cf::string_text(kSecAttrAccount).unwrap(),
            cf::string_text(kSecClassGenericPassword).unwrap(),
        )
    };
    let expected = BTreeMap::from([
        (class, Value::String(generic)),
        (service, Value::String("rc-git".to_owned())),
        (
            account,
            Value::String("github-api/ghe.example.com:8443/oc".to_owned()),
        ),
    ]);
    assert_eq!(map, expected);
}

#[test]
fn only_rc_git_prefixed_services_are_accepted() {
    assert!(KeychainApiTokenStore::with_service("rc-git-test-1").is_ok());
    for bad in [
        "rc-git",
        "rc-git-",
        "other",
        "rc-git-a/b",
        "rc-git-a b",
        "Safari",
    ] {
        assert!(KeychainApiTokenStore::with_service(bad).is_err(), "{bad}");
    }
}

#[test]
fn tokens_round_trip_replace_in_place_and_delete() {
    let scratch = Scratch::new();
    let (store, key) = (&scratch.store, &scratch.key);
    assert_eq!(store.tokens(key).unwrap(), None);
    store
        .store_tokens(key, &app_tokens("ghu_one", "ghr_one"))
        .unwrap();
    assert_eq!(
        store.tokens(key).unwrap(),
        Some(app_tokens("ghu_one", "ghr_one"))
    );
    store
        .store_tokens(key, &app_tokens("ghu_two", "ghr_two"))
        .unwrap();
    assert_eq!(
        store.tokens(key).unwrap(),
        Some(app_tokens("ghu_two", "ghr_two")),
        "a store replaces the earlier pair"
    );
    let other = AccountKey::new(key.host().clone(), "someone-else").unwrap();
    assert_eq!(
        store.tokens(&other).unwrap(),
        None,
        "another account reads nothing"
    );
    assert_eq!(
        KeychainApiTokenStore::new().tokens(key).unwrap(),
        None,
        "another service's item is never read"
    );
    store.delete_tokens(key).unwrap();
    store.delete_tokens(key).unwrap();
    assert_eq!(store.tokens(key).unwrap(), None);
}

#[test]
fn api_tokens_and_push_tokens_of_one_host_never_touch_each_other() {
    let scratch = Scratch::new();
    let push = KeychainCredentialStore::with_security_domain(&format!("rc-git-test-{}", unique()));
    let host = scratch.key.host().to_string();
    let push_token = Credentials {
        username: "git".to_owned(),
        token: "push-token".to_owned(),
    };
    scratch
        .store
        .store_tokens(&scratch.key, &app_tokens("ghu_api", "ghr_api"))
        .unwrap();
    push.store(&push_token, &host, CredentialKind::HttpsToken)
        .unwrap();
    push.store(&push_token, &host, CredentialKind::HttpsToken)
        .unwrap();
    assert_eq!(
        scratch.store.tokens(&scratch.key).unwrap(),
        Some(app_tokens("ghu_api", "ghr_api")),
        "replacing the push token kept the API token"
    );
    scratch.store.delete_tokens(&scratch.key).unwrap();
    assert_eq!(
        push.credentials(&host, CredentialKind::HttpsToken).unwrap(),
        Some(push_token),
        "deleting the API token kept the push token"
    );
    push.delete_item(&host, CredentialKind::HttpsToken).unwrap();
}
