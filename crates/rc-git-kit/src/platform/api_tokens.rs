//! Where GitHub API tokens live: one secret per host and account, holding the access token and,
//! for a GitHub App, its refresh token and both expiries. The login-keychain store lives in
//! `rc-git-mac`, apart from the HTTPS push tokens of [`super::credentials`]; tests and models use
//! [`MemoryApiTokenStore`].
//!
//! The account key decides where its token may go: the API base is derived from the key's
//! host alone, so a settings record naming some other host for a login reads no token at all.
//! The stored bytes are [`encode_tokens`]'s line format; nothing else reads them.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rc_git_guard::actions::github::{ApiToken, GitHubHost, RefreshToken, TokenSet};

use super::credentials::CredentialError;

/// One account on one host: the key its token is stored under.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountKey {
    host: GitHubHost,
    login: String,
}

impl AccountKey {
    /// `login` on `host`. A login is 1 to 100 letters, digits, `.`, `_` or `-`.
    ///
    /// # Errors
    /// The refusal to show, naming the login, when it has another shape.
    pub fn new(host: GitHubHost, login: &str) -> Result<Self, String> {
        let plain = login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if login.is_empty() || login.len() > 100 || !plain || login == "." || login == ".." {
            return Err(format!(
                "'{}' is not a GitHub login",
                rc_git_guard::actions::github::single_line(login, 60)
            ));
        }
        Ok(Self {
            host,
            login: login.to_owned(),
        })
    }

    /// The host.
    #[must_use]
    pub fn host(&self) -> &GitHubHost {
        &self.host
    }

    /// The login.
    #[must_use]
    pub fn login(&self) -> &str {
        &self.login
    }

    /// `host/login`: how the per-repository choice names the account. Neither part holds a
    /// `/`, so the split is unambiguous.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}/{}", self.host, self.login)
    }

    /// The account [`AccountKey::id`] spelled, `None` for any other text.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        let (host, login) = id.split_once('/')?;
        Self::new(GitHubHost::parse(host).ok()?, login).ok()
    }
}

impl fmt::Display for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on {}", self.login, self.host)
    }
}

/// Stores one account's tokens per [`AccountKey`].
pub trait ApiTokenStore: Send + Sync {
    /// The tokens saved for `account`, or `None` when there are none.
    ///
    /// # Errors
    /// [`CredentialError`] with op `"read"` naming the account when the store could not
    /// answer or holds bytes [`decode_tokens`] refuses.
    fn tokens(&self, account: &AccountKey) -> Result<Option<TokenSet>, CredentialError>;

    /// Saves `tokens` for `account`, replacing what was there in one step.
    ///
    /// # Errors
    /// [`CredentialError`] with op `"write"` naming the account when the store refused it.
    fn store_tokens(&self, account: &AccountKey, tokens: &TokenSet) -> Result<(), CredentialError>;

    /// Forgets `account`'s tokens; forgetting what is not there succeeds.
    ///
    /// # Errors
    /// [`CredentialError`] with op `"delete"` naming the account when the store refused it.
    fn delete_tokens(&self, account: &AccountKey) -> Result<(), CredentialError>;
}

impl<T: ApiTokenStore + ?Sized> ApiTokenStore for &T {
    fn tokens(&self, account: &AccountKey) -> Result<Option<TokenSet>, CredentialError> {
        (**self).tokens(account)
    }

    fn store_tokens(&self, account: &AccountKey, tokens: &TokenSet) -> Result<(), CredentialError> {
        (**self).store_tokens(account, tokens)
    }

    fn delete_tokens(&self, account: &AccountKey) -> Result<(), CredentialError> {
        (**self).delete_tokens(account)
    }
}

impl<T: ApiTokenStore + ?Sized> ApiTokenStore for std::sync::Arc<T> {
    fn tokens(&self, account: &AccountKey) -> Result<Option<TokenSet>, CredentialError> {
        (**self).tokens(account)
    }

    fn store_tokens(&self, account: &AccountKey, tokens: &TokenSet) -> Result<(), CredentialError> {
        (**self).store_tokens(account, tokens)
    }

    fn delete_tokens(&self, account: &AccountKey) -> Result<(), CredentialError> {
        (**self).delete_tokens(account)
    }
}

/// The first line of every stored token set.
const FORMAT_LINE: &str = "rc-git github tokens 1";

/// `tokens` as the bytes a store keeps: a format line, then one `name value` line per field.
/// Tokens are visible ASCII without spaces, so no field can spill into the next.
#[must_use]
pub fn encode_tokens(tokens: &TokenSet) -> Vec<u8> {
    let mut lines = vec![
        FORMAT_LINE.to_owned(),
        format!("access {}", tokens.access.expose_secret()),
    ];
    if let Some(at) = tokens.access_expires_at {
        lines.push(format!("access-expires {at}"));
    }
    if let Some(refresh) = &tokens.refresh {
        lines.push(format!("refresh {}", refresh.expose_secret()));
    }
    if let Some(at) = tokens.refresh_expires_at {
        lines.push(format!("refresh-expires {at}"));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text.into_bytes()
}

/// The token set [`encode_tokens`] wrote; `None` for any other bytes: another format line, an
/// unknown or repeated field, a value that is not a token or a number, or no access token.
#[must_use]
pub fn decode_tokens(bytes: &[u8]) -> Option<TokenSet> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != FORMAT_LINE {
        return None;
    }
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in lines {
        let (name, value) = line.split_once(' ')?;
        if fields.insert(name, value).is_some() {
            return None;
        }
    }
    let number = |name: &str| -> Option<Option<u64>> {
        match fields.get(name) {
            Some(value) => value.parse().ok().map(Some),
            None => Some(None),
        }
    };
    let known = ["access", "access-expires", "refresh", "refresh-expires"];
    if fields.keys().any(|name| !known.contains(name)) {
        return None;
    }
    let access = ApiToken::new((*fields.get("access")?).to_owned()).ok()?;
    let refresh = match fields.get("refresh") {
        Some(value) => Some(RefreshToken::new((*value).to_owned()).ok()?),
        None => None,
    };
    Some(TokenSet {
        access,
        access_expires_at: number("access-expires")?,
        refresh,
        refresh_expires_at: number("refresh-expires")?,
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An [`ApiTokenStore`] in memory. Counts reads and writes and can fail them, so a model test
/// can assert how often the keychain would be asked.
#[derive(Default)]
pub struct MemoryApiTokenStore {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    items: HashMap<AccountKey, Vec<u8>>,
    reads: usize,
    writes: usize,
    failing_writes: bool,
}

impl MemoryApiTokenStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many reads reached the store.
    #[must_use]
    pub fn read_count(&self) -> usize {
        lock(&self.state).reads
    }

    /// How many writes reached the store.
    #[must_use]
    pub fn write_count(&self) -> usize {
        lock(&self.state).writes
    }

    /// Every later write fails (or succeeds again).
    pub fn fail_writes(&self, fail: bool) {
        lock(&self.state).failing_writes = fail;
    }

    /// The accounts holding tokens.
    #[must_use]
    pub fn accounts(&self) -> Vec<AccountKey> {
        let mut keys: Vec<AccountKey> = lock(&self.state).items.keys().cloned().collect();
        keys.sort();
        keys
    }
}

impl fmt::Debug for MemoryApiTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("MemoryApiTokenStore")
            .field("items", &state.items.len())
            .field("secrets", &"<redacted>")
            .finish_non_exhaustive()
    }
}

fn failed(op: &'static str, account: &AccountKey, status: i32) -> CredentialError {
    CredentialError {
        op,
        host: account.to_string(),
        status,
    }
}

impl ApiTokenStore for MemoryApiTokenStore {
    fn tokens(&self, account: &AccountKey) -> Result<Option<TokenSet>, CredentialError> {
        let mut state = lock(&self.state);
        state.reads += 1;
        let Some(bytes) = state.items.get(account) else {
            return Ok(None);
        };
        decode_tokens(bytes)
            .map(Some)
            .ok_or_else(|| failed("read", account, -26275))
    }

    fn store_tokens(&self, account: &AccountKey, tokens: &TokenSet) -> Result<(), CredentialError> {
        let mut state = lock(&self.state);
        state.writes += 1;
        if state.failing_writes {
            return Err(failed("write", account, -25293));
        }
        state.items.insert(account.clone(), encode_tokens(tokens));
        Ok(())
    }

    fn delete_tokens(&self, account: &AccountKey) -> Result<(), CredentialError> {
        let mut state = lock(&self.state);
        state.writes += 1;
        if state.failing_writes {
            return Err(failed("delete", account, -25293));
        }
        state.items.remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> TokenSet {
        TokenSet {
            access: ApiToken::new("ghu_access".to_owned()).unwrap(),
            access_expires_at: Some(1_800_000_000),
            refresh: Some(RefreshToken::new("ghr_refresh".to_owned()).unwrap()),
            refresh_expires_at: Some(1_900_000_000),
        }
    }

    #[test]
    fn token_sets_round_trip_through_the_stored_format() {
        for tokens in [
            full(),
            TokenSet::lasting(ApiToken::new("ghp_x".to_owned()).unwrap()),
        ] {
            assert_eq!(decode_tokens(&encode_tokens(&tokens)), Some(tokens));
        }
        assert_eq!(
            String::from_utf8(encode_tokens(&full())).unwrap(),
            "rc-git github tokens 1\naccess ghu_access\naccess-expires 1800000000\n\
             refresh ghr_refresh\nrefresh-expires 1900000000\n"
        );
    }

    #[test]
    fn anything_but_the_written_format_decodes_to_nothing() {
        for bad in [
            "",
            "access ghu_x\n",
            "rc-git github tokens 2\naccess ghu_x\n",
            "rc-git github tokens 1\n",
            "rc-git github tokens 1\naccess ghu_x\naccess ghu_y\n",
            "rc-git github tokens 1\naccess ghu_x\nhost evil.example\n",
            "rc-git github tokens 1\naccess ghu_x\naccess-expires soon\n",
            "rc-git github tokens 1\naccess ghu x\n",
        ] {
            assert_eq!(decode_tokens(bad.as_bytes()), None, "{bad:?}");
        }
    }

    #[test]
    fn account_ids_round_trip_and_refuse_other_shapes() {
        let key = AccountKey::new(
            GitHubHost::parse("ghe.example.com:8443").unwrap(),
            "oc-to_1",
        )
        .unwrap();
        assert_eq!(key.id(), "ghe.example.com:8443/oc-to_1");
        assert_eq!(AccountKey::from_id(&key.id()), Some(key));
        for bad in ["", "a/b", "..", "a b", "é"] {
            assert!(
                AccountKey::new(GitHubHost::github_com(), bad).is_err(),
                "{bad:?}"
            );
        }
        for bad in [
            "github.com",
            "github.com/",
            "evil.example/x/y",
            "https://x/y",
        ] {
            assert_eq!(AccountKey::from_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_memory_store_never_prints_a_token() {
        let store = MemoryApiTokenStore::new();
        let key = AccountKey::new(GitHubHost::github_com(), "octocat").unwrap();
        store.store_tokens(&key, &full()).unwrap();
        let printed = format!("{store:?}");
        assert!(!printed.contains("ghu_access"), "{printed}");
        assert_eq!(store.tokens(&key).unwrap(), Some(full()));
        store.delete_tokens(&key).unwrap();
        store.delete_tokens(&key).unwrap();
        assert_eq!(store.tokens(&key).unwrap(), None);
    }
}
