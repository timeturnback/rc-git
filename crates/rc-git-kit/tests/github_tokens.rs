//! The token keeper: a GitHub App token renews once, however many callers race for it, and a
//! refused refresh signs out only when no other writer renewed the token first.
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use rc_git_kit::github::accounts::{GitHubAccount, SignInMethod};
use rc_git_kit::github::tokens::{EXPIRY_MARGIN_SECS, TokenError, TokenKeeper};
use rc_git_kit::github::{ApiToken, ClientId, GitHubError, GitHubHost, RefreshToken, TokenSet};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};

const NOW: u64 = 1_800_000_000;

fn account() -> GitHubAccount {
    GitHubAccount {
        key: AccountKey::new(GitHubHost::github_com(), "octocat").unwrap(),
        method: SignInMethod::GitHubApp,
        client_id: Some(ClientId::parse("Iv23li.client").unwrap()),
        scopes: None,
        token_expires: None,
    }
}

fn pair(access: &str, expires_at: u64, refresh: &str) -> TokenSet {
    TokenSet {
        access: ApiToken::new(access.to_owned()).unwrap(),
        access_expires_at: Some(expires_at),
        refresh: Some(RefreshToken::new(refresh.to_owned()).unwrap()),
        refresh_expires_at: Some(NOW + 1_000_000),
    }
}

fn keeper_holding(tokens: &TokenSet) -> TokenKeeper<MemoryApiTokenStore> {
    let store = MemoryApiTokenStore::new();
    store.store_tokens(&account().key, tokens).unwrap();
    TokenKeeper::new(store)
}

fn rejected() -> GitHubError {
    GitHubError::RefreshRejected {
        host: "github.com".to_owned(),
        code: "bad_refresh_token".to_owned(),
    }
}

/// A refresh that must not run: it fails with an error no assertion here accepts.
fn never(_: &RefreshToken) -> Result<TokenSet, GitHubError> {
    Err(GitHubError::Malformed {
        op: "refresh",
        url: String::new(),
        reason: "a refresh ran that the test forbids".to_owned(),
    })
}

#[test]
fn a_fresh_token_is_read_once_and_never_refreshed() {
    let keeper = keeper_holding(&pair("ghu_fresh", NOW + 3600, "ghr_one"));
    for _ in 0..3 {
        let fresh = keeper.token(&account(), NOW, never).unwrap();
        assert_eq!(fresh.token.expose_secret(), "ghu_fresh");
    }
    assert_eq!(keeper.store().read_count(), 1);
}

#[test]
fn a_token_inside_the_expiry_margin_is_renewed_and_stored() {
    let keeper = keeper_holding(&pair("ghu_old", NOW + EXPIRY_MARGIN_SECS, "ghr_one"));
    let fresh = keeper
        .token(&account(), NOW, |used| {
            assert_eq!(used.expose_secret(), "ghr_one");
            Ok(pair("ghu_new", NOW + 28_800, "ghr_two"))
        })
        .unwrap();
    assert_eq!(
        (fresh.token.expose_secret(), fresh.unsaved),
        ("ghu_new", None)
    );
    let stored = keeper.store().tokens(&account().key).unwrap().unwrap();
    assert_eq!(stored, pair("ghu_new", NOW + 28_800, "ghr_two"));
}

#[test]
fn racing_callers_spend_the_single_use_refresh_token_once() {
    let keeper = Arc::new(keeper_holding(&pair("ghu_old", NOW - 1, "ghr_one")));
    let spent = Arc::new(Mutex::new(Vec::<String>::new()));
    let callers = 8;
    let start = Arc::new(Barrier::new(callers));
    let mut threads = Vec::new();
    for _ in 0..callers {
        let (keeper, spent, start) = (Arc::clone(&keeper), Arc::clone(&spent), Arc::clone(&start));
        threads.push(std::thread::spawn(move || {
            start.wait();
            keeper.token(&account(), NOW, |used| {
                let mut spent = spent.lock().unwrap();
                let reused = spent.iter().any(|token| token == used.expose_secret());
                spent.push(used.expose_secret().to_owned());
                if reused {
                    return Err(rejected());
                }
                drop(spent);
                std::thread::sleep(Duration::from_millis(50));
                Ok(pair("ghu_new", NOW + 28_800, "ghr_two"))
            })
        }));
    }
    for thread in threads {
        let fresh = thread.join().unwrap().unwrap();
        assert_eq!(fresh.token.expose_secret(), "ghu_new");
    }
    assert_eq!(
        *spent.lock().unwrap(),
        vec!["ghr_one".to_owned()],
        "one refresh ran; every other caller found its result"
    );
}

#[test]
fn a_refused_refresh_signs_out_only_when_the_store_still_holds_it() {
    let keeper = keeper_holding(&pair("ghu_old", NOW - 1, "ghr_one"));
    let error = keeper
        .token(&account(), NOW, |_| Err(rejected()))
        .unwrap_err();
    assert!(matches!(error, TokenError::SignedOut { .. }), "{error}");
    assert_eq!(
        keeper.store().read_count(),
        2,
        "the store is read again before signing out"
    );
}

#[test]
fn a_refresh_another_writer_made_first_is_used_instead_of_signing_out() {
    let store = Arc::new(MemoryApiTokenStore::new());
    store
        .store_tokens(&account().key, &pair("ghu_old", NOW - 1, "ghr_one"))
        .unwrap();
    let keeper = TokenKeeper::new(Arc::clone(&store));
    let fresh = keeper
        .token(&account(), NOW, |_| {
            // The CLI renewed with the same refresh token a moment earlier.
            store
                .store_tokens(&account().key, &pair("ghu_cli", NOW + 28_800, "ghr_cli"))
                .unwrap();
            Err(rejected())
        })
        .unwrap();
    assert_eq!(fresh.token.expose_secret(), "ghu_cli");
}

#[test]
fn a_network_failure_is_reported_and_signs_nobody_out() {
    let keeper = keeper_holding(&pair("ghu_old", NOW - 1, "ghr_one"));
    let error = keeper
        .token(&account(), NOW, |_| {
            Err(GitHubError::Transport {
                op: "refresh the token",
                url: "https://github.com/login/oauth/access_token".to_owned(),
                reason: "Couldn't connect to server".to_owned(),
            })
        })
        .unwrap_err();
    assert!(
        matches!(error, TokenError::GitHub(GitHubError::Transport { .. })),
        "{error}"
    );
    let stored = keeper.store().tokens(&account().key).unwrap().unwrap();
    assert_eq!(stored, pair("ghu_old", NOW - 1, "ghr_one"));
}

#[test]
fn an_expired_token_without_a_usable_refresh_token_is_signed_out() {
    let mut lasting = pair("ghu_old", NOW - 1, "ghr_one");
    lasting.refresh = None;
    let keeper = keeper_holding(&lasting);
    let error = keeper.token(&account(), NOW, never).unwrap_err();
    assert!(matches!(error, TokenError::SignedOut { .. }), "{error}");

    let mut stale = pair("ghu_old", NOW - 1, "ghr_one");
    stale.refresh_expires_at = Some(NOW);
    let keeper = keeper_holding(&stale);
    let error = keeper.token(&account(), NOW, never).unwrap_err();
    assert!(matches!(error, TokenError::SignedOut { .. }), "{error}");

    let empty = TokenKeeper::new(MemoryApiTokenStore::new());
    let error = empty.token(&account(), NOW, never).unwrap_err();
    assert!(matches!(error, TokenError::SignedOut { .. }), "{error}");
}

#[test]
fn a_renewal_the_store_refuses_still_serves_this_session() {
    let keeper = keeper_holding(&pair("ghu_old", NOW - 1, "ghr_one"));
    keeper.store().fail_writes(true);
    let calls = AtomicUsize::new(0);
    let fresh = keeper
        .token(&account(), NOW, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(pair("ghu_new", NOW + 28_800, "ghr_two"))
        })
        .unwrap();
    assert_eq!(fresh.token.expose_secret(), "ghu_new");
    assert!(fresh.unsaved.is_some());
    let again = keeper.token(&account(), NOW, never).unwrap();
    assert_eq!(again.token.expose_secret(), "ghu_new");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_keeper_never_prints_a_token() {
    let keeper = keeper_holding(&pair("ghu_secret_value", NOW + 3600, "ghr_secret_value"));
    keeper.token(&account(), NOW, never).unwrap();
    let printed = format!("{keeper:?}");
    assert!(!printed.contains("secret_value"), "{printed}");
}
