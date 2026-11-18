//! GitHub accounts in settings, and which account a repository uses (one rule, no fallthrough).
#![expect(
    clippy::unwrap_used,
    reason = "the fixture helpers outside #[test] fns unwrap values valid by construction"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use rc_git_kit::github::accounts::{
    AccountChoice, GitHubAccount, GitHubAccounts, SignInMethod, account_for, account_for_remote,
};
use rc_git_kit::github::{ApiToken, ClientId, GitHubHost, TokenSet, save_sign_in, sign_out};
use rc_git_kit::platform::api_tokens::{AccountKey, ApiTokenStore, MemoryApiTokenStore};
use rc_git_kit::platform::keys;
use rc_git_kit::platform::settings::{MemorySettings, SettingsStore, Value};

fn account(host: &str, login: &str) -> GitHubAccount {
    GitHubAccount {
        key: AccountKey::new(GitHubHost::parse(host).unwrap(), login).unwrap(),
        method: SignInMethod::GitHubApp,
        client_id: Some(ClientId::parse("Iv23li.client").unwrap()),
        scopes: None,
        token_expires: None,
    }
}

fn accounts() -> (Arc<MemorySettings>, GitHubAccounts) {
    let settings = Arc::new(MemorySettings::new());
    let store = GitHubAccounts::new(Arc::clone(&settings) as Arc<dyn SettingsStore>);
    (settings, store)
}

#[test]
fn accounts_round_trip_through_settings_sorted_and_without_duplicates() {
    let (_, store) = accounts();
    let mut token_account = account("github.com", "zed");
    token_account.method = SignInMethod::Token;
    token_account.client_id = None;
    token_account.scopes = Some("repo".to_owned());
    token_account.token_expires = Some("2026-12-01 10:00:00 UTC".to_owned());
    store.save(&token_account).unwrap();
    store.save(&account("github.com", "octocat")).unwrap();
    store
        .save(&account("ghe.example.com:8443", "octocat"))
        .unwrap();
    store.save(&account("github.com", "octocat")).unwrap();
    let loaded = store.load();
    assert_eq!(
        loaded,
        vec![
            account("ghe.example.com:8443", "octocat"),
            account("github.com", "octocat"),
            token_account
        ]
    );
}

#[test]
fn a_malformed_record_is_dropped_alone() {
    let (settings, store) = accounts();
    store.save(&account("github.com", "octocat")).unwrap();
    let Some(Value::Array(mut items)) = settings.value(keys::GITHUB_ACCOUNTS) else {
        panic!("accounts are a list");
    };
    let mut bad_host = BTreeMap::new();
    bad_host.insert(
        "host".to_owned(),
        Value::String("https://evil.example".to_owned()),
    );
    bad_host.insert("login".to_owned(), Value::String("x".to_owned()));
    bad_host.insert("method".to_owned(), Value::String("token".to_owned()));
    let mut no_client = BTreeMap::new();
    no_client.insert("host".to_owned(), Value::String("github.com".to_owned()));
    no_client.insert("login".to_owned(), Value::String("y".to_owned()));
    no_client.insert("method".to_owned(), Value::String("github-app".to_owned()));
    items.extend([Value::Dict(bad_host), Value::Dict(no_client), Value::Int(3)]);
    settings
        .set_value(keys::GITHUB_ACCOUNTS, Some(Value::Array(items)))
        .unwrap();
    assert_eq!(store.load(), vec![account("github.com", "octocat")]);
}

#[test]
fn free_text_from_a_tampered_settings_file_loads_on_one_visible_line() {
    let (settings, store) = accounts();
    let mut record = BTreeMap::new();
    let mut put = |name: &str, value: &str| {
        record.insert(name.to_owned(), Value::String(value.to_owned()));
    };
    put("host", "github.com");
    put("login", "octocat");
    put("method", "token");
    put("scopes", "repo\u{1b}]0;owned\u{7}\u{1b}[2J\nX-Injected: 1");
    put("tokenExpires", "soon\u{202e}txt.exe\u{2028}");
    settings
        .set_value(
            keys::GITHUB_ACCOUNTS,
            Some(Value::Array(vec![Value::Dict(record)])),
        )
        .unwrap();
    let [loaded] = store.load().try_into().unwrap();
    assert_eq!(
        loaded.scopes.as_deref(),
        Some("repo\\u{a}X-Injected: 1"),
        "escape sequences dropped, the line break shown"
    );
    assert_eq!(
        loaded.token_expires.as_deref(),
        Some("soon\\u{202e}txt.exe\\u{2028}")
    );
}

#[test]
fn signing_in_keeps_the_token_in_the_token_store_and_never_in_settings() {
    let (settings, store) = accounts();
    let tokens = MemoryApiTokenStore::new();
    let octocat = account("github.com", "octocat");
    let set = TokenSet::lasting(ApiToken::new("ghu_never_in_settings".to_owned()).unwrap());
    save_sign_in(&tokens, &store, &octocat, &set).unwrap();
    assert_eq!(tokens.tokens(&octocat.key).unwrap(), Some(set));
    let printed = format!("{:?}", settings.value(keys::GITHUB_ACCOUNTS));
    assert!(!printed.contains("ghu_never_in_settings"), "{printed}");
    assert!(printed.contains("octocat"), "{printed}");
}

#[test]
fn a_failed_token_write_saves_no_account() {
    let (_, store) = accounts();
    let tokens = MemoryApiTokenStore::new();
    tokens.fail_writes(true);
    let set = TokenSet::lasting(ApiToken::new("ghu_x".to_owned()).unwrap());
    let error = save_sign_in(&tokens, &store, &account("github.com", "octocat"), &set);
    assert!(error.is_err());
    assert!(store.load().is_empty());
}

#[test]
fn signing_out_forgets_the_record_its_choices_and_its_token() {
    let (_, store) = accounts();
    let tokens = MemoryApiTokenStore::new();
    let (first, second) = (
        account("github.com", "first"),
        account("github.com", "second"),
    );
    let set = TokenSet::lasting(ApiToken::new("ghu_x".to_owned()).unwrap());
    save_sign_in(&tokens, &store, &first, &set).unwrap();
    save_sign_in(&tokens, &store, &second, &set).unwrap();
    store.choose("/work/a", &first.key).unwrap();
    store.choose("/work/b", &second.key).unwrap();
    sign_out(&tokens, &store, &first.key).unwrap();
    assert_eq!(store.load(), vec![second.clone()]);
    assert_eq!(
        store.choices(),
        BTreeMap::from([("/work/b".to_owned(), second.key.id())])
    );
    assert_eq!(tokens.accounts(), vec![second.key]);
}

#[test]
fn one_account_on_the_host_is_used_and_none_is_no_account() {
    let com = GitHubHost::github_com();
    let only = vec![
        account("github.com", "octocat"),
        account("ghe.example.com", "work"),
    ];
    assert_eq!(
        account_for(&com, "/r", &only, &BTreeMap::new()),
        AccountChoice::Use(account("github.com", "octocat"))
    );
    let ghe_ported = GitHubHost::parse("ghe.example.com:8443").unwrap();
    assert_eq!(
        account_for(&ghe_ported, "/r", &only, &BTreeMap::new()),
        AccountChoice::NoAccount { host: ghe_ported }
    );
}

#[test]
fn several_accounts_on_the_host_ask_unless_the_repository_chose_one() {
    let com = GitHubHost::github_com();
    let several = vec![
        account("github.com", "home"),
        account("github.com", "work"),
        account("ghe.example.com", "other"),
    ];
    let none = BTreeMap::new();
    assert_eq!(
        account_for(&com, "/r", &several, &none),
        AccountChoice::Ask(vec![
            account("github.com", "home"),
            account("github.com", "work")
        ])
    );
    let chose = BTreeMap::from([("/r".to_owned(), "github.com/work".to_owned())]);
    assert_eq!(
        account_for(&com, "/r", &several, &chose),
        AccountChoice::Use(account("github.com", "work"))
    );
    assert!(matches!(
        account_for(&com, "/other", &several, &chose),
        AccountChoice::Ask(_)
    ));
    for stale in ["github.com/gone", "ghe.example.com/other", "garbage"] {
        let choices = BTreeMap::from([("/r".to_owned(), stale.to_owned())]);
        assert!(
            matches!(
                account_for(&com, "/r", &several, &choices),
                AccountChoice::Ask(_)
            ),
            "{stale}"
        );
    }
}

#[test]
fn a_hostile_remote_never_selects_an_account_of_another_host() {
    let signed_in = vec![account("github.com", "octocat")];
    let none = BTreeMap::new();
    for remote in [
        "https://github.com.evil.example/o/r.git",
        "https://evilgithub.com/o/r.git",
        "https://sub.github.com/o/r.git",
        "https://evil.example/github.com/o/r.git",
        "git@github.com.evil.example:o/r.git",
    ] {
        let choice = account_for_remote(remote, "/r", &signed_in, &none);
        assert!(
            matches!(choice, Ok(AccountChoice::NoAccount { .. }) | Err(_)),
            "{remote}: {choice:?}"
        );
    }
    for remote in [
        "https://user:pw@github.com/o/r.git",
        "git://github.com/o/r.git",
    ] {
        assert!(
            account_for_remote(remote, "/r", &signed_in, &none).is_err(),
            "{remote}"
        );
    }
    assert_eq!(
        account_for_remote("git@github.com:o/r.git", "/r", &signed_in, &none).unwrap(),
        AccountChoice::Use(account("github.com", "octocat"))
    );
}
