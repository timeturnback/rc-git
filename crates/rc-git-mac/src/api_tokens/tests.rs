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

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecAttrComment: CFTypeRef;
}

/// What another program could leave behind: an item under the scratch store's service and
/// account, marked with a comment rc-git never writes.
fn plant(scratch: &Scratch) {
    let parts = scratch.store.parts(&scratch.key, "plant").unwrap();
    let data = cf::data(b"planted").unwrap();
    let comment = cf::string("planted by another program").unwrap();
    let mut pairs = KeychainApiTokenStore::identity(&parts);
    // SAFETY: framework constants.
    unsafe {
        pairs.push((kSecValueData, data.as_ptr()));
        pairs.push((kSecAttrComment, comment.as_ptr()));
    }
    let attributes = cf::dictionary(&pairs).unwrap();
    // SAFETY: `attributes` is live; a null result pointer asks for nothing back.
    let status = unsafe { SecItemAdd(attributes.as_ptr(), ptr::null_mut()) };
    assert_eq!(status, ERR_SEC_SUCCESS, "plant the item");
}

/// The comment on the scratch store's item; `None` when the item has none.
fn comment(scratch: &Scratch) -> Option<String> {
    let parts = scratch.store.parts(&scratch.key, "inspect").unwrap();
    let mut pairs = KeychainApiTokenStore::identity(&parts);
    // SAFETY: framework constants.
    unsafe {
        pairs.push((kSecReturnAttributes, cf::boolean(true)));
        pairs.push((kSecMatchLimit, kSecMatchLimitOne));
    }
    let query = cf::dictionary(&pairs).unwrap();
    let mut result: CFTypeRef = ptr::null();
    // SAFETY: `query` is live; on success `result` receives an owned dictionary.
    let status = unsafe { SecItemCopyMatching(query.as_ptr(), &raw mut result) };
    assert_eq!(status, ERR_SEC_SUCCESS, "find the item");
    // SAFETY: `result` is the owned reference the call returned.
    let item = unsafe { Owned::from_create(result) }.unwrap();
    // SAFETY: `item` is a live dictionary; the value is borrowed from it while it lives.
    let comment = unsafe { cf::dictionary_get(item.as_ptr(), kSecAttrComment) };
    // SAFETY: a comment attribute is a CF string.
    (!comment.is_null()).then(|| unsafe { cf::string_text(comment) }.unwrap())
}

#[test]
fn a_sign_in_replaces_a_planted_item_with_a_fresh_one() {
    let scratch = Scratch::new();
    plant(&scratch);
    assert_eq!(
        comment(&scratch).as_deref(),
        Some("planted by another program")
    );
    scratch
        .store
        .save_new_tokens(&scratch.key, &app_tokens("ghu_user", "ghr_user"))
        .unwrap();
    assert_eq!(
        comment(&scratch),
        None,
        "the token went into a new item, not the planted one"
    );
    assert_eq!(
        scratch.store.tokens(&scratch.key).unwrap(),
        Some(app_tokens("ghu_user", "ghr_user"))
    );
}

#[test]
fn a_refresh_writes_only_into_the_item_that_is_there() {
    let scratch = Scratch::new();
    let (store, key) = (&scratch.store, &scratch.key);
    assert_eq!(
        store.update_tokens(key, &app_tokens("ghu_x", "ghr_x")),
        Ok(TokenUpdate::NoItem)
    );
    assert_eq!(store.tokens(key).unwrap(), None, "a refresh adds no item");
    plant(&scratch);
    assert_eq!(
        store.update_tokens(key, &app_tokens("ghu_x", "ghr_x")),
        Ok(TokenUpdate::Replaced)
    );
    assert_eq!(
        comment(&scratch).as_deref(),
        Some("planted by another program"),
        "an update keeps the item it found, which is why a sign-in never takes this path"
    );
}

#[test]
fn tokens_round_trip_replace_in_place_and_delete() {
    let scratch = Scratch::new();
    let (store, key) = (&scratch.store, &scratch.key);
    assert_eq!(store.tokens(key).unwrap(), None);
    store
        .save_new_tokens(key, &app_tokens("ghu_one", "ghr_one"))
        .unwrap();
    assert_eq!(
        store.tokens(key).unwrap(),
        Some(app_tokens("ghu_one", "ghr_one"))
    );
    assert_eq!(
        store.update_tokens(key, &app_tokens("ghu_two", "ghr_two")),
        Ok(TokenUpdate::Replaced)
    );
    assert_eq!(
        store.tokens(key).unwrap(),
        Some(app_tokens("ghu_two", "ghr_two")),
        "a refresh replaces the earlier pair"
    );
    store
        .save_new_tokens(key, &app_tokens("ghu_three", "ghr_three"))
        .unwrap();
    assert_eq!(
        store.tokens(key).unwrap(),
        Some(app_tokens("ghu_three", "ghr_three")),
        "a second sign-in replaces the item"
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
        .save_new_tokens(&scratch.key, &app_tokens("ghu_api", "ghr_api"))
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
