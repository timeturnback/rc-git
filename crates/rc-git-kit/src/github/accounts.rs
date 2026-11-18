//! The GitHub accounts the user signed into, and which one a repository uses. Both live in
//! app settings, never in a repository's own config: a repository someone else produced must
//! not pick the account its pull requests are read or merged with.
//!
//! A repository's account follows one rule ([`account_for`]): no account on its host, none;
//! one, that one; several, the one the user chose for that repository, else ask. A second
//! account's token is never tried against a repository.

use std::collections::BTreeMap;

use rc_git_guard::actions::github::{ClientId, GitHubError, GitHubHost, remote_host, single_line};

use crate::platform::api_tokens::AccountKey;
use crate::platform::keys;
use crate::platform::settings::{SettingsError, Value};
use crate::platform::stores::SharedSettings;

/// How an account signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignInMethod {
    /// A GitHub App's device flow (the default): fine-grained permissions, tokens that renew.
    GitHubApp,
    /// An OAuth App's device flow: the `repo` scope.
    OAuthApp,
    /// A pasted personal access token, fine-grained preferably.
    Token,
}

impl SignInMethod {
    /// Every method, in the order Settings offers them.
    pub const ALL: [Self; 3] = [Self::GitHubApp, Self::OAuthApp, Self::Token];

    /// The word settings and the CLI store and accept.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::GitHubApp => "github-app",
            Self::OAuthApp => "oauth-app",
            Self::Token => "token",
        }
    }

    /// The method [`SignInMethod::word`] spells.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.word() == word)
    }

    /// Whether the method signs in through a device flow, which needs a client id.
    #[must_use]
    pub fn uses_device_flow(self) -> bool {
        match self {
            Self::GitHubApp | Self::OAuthApp => true,
            Self::Token => false,
        }
    }
}

/// One account the user signed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubAccount {
    /// The host and login: the key of its token.
    pub key: AccountKey,
    /// How it signed in.
    pub method: SignInMethod,
    /// The app it signed in through; a device-flow account's refresh needs it.
    pub client_id: Option<ClientId>,
    /// The scopes the host reported at sign-in, when it reported any.
    pub scopes: Option<String>,
    /// When the host said the token expires, as it said it, when it did.
    pub token_expires: Option<String>,
}

/// The most characters of a free-text field (the scopes, the expiry) a record keeps, as
/// `validate_token` reports them.
const FREE_TEXT_CHARS: usize = 200;

/// Reads one account record; `None` when any required field is missing or malformed, so one
/// bad record hides no other. The settings file is outside rc-git's control (another program
/// can edit it), so the host, login, method and client id must parse, and the free-text
/// scopes and expiry pass through [`single_line`]: no escape sequence or control character
/// in them reaches a terminal or the UI.
fn account_from(value: &Value) -> Option<GitHubAccount> {
    let Value::Dict(fields) = value else {
        return None;
    };
    let text = |name: &str| fields.get(name).and_then(Value::as_str);
    let shown = |name: &str| text(name).map(|value| single_line(value, FREE_TEXT_CHARS));
    let host = GitHubHost::parse(text("host")?).ok()?;
    let key = AccountKey::new(host, text("login")?).ok()?;
    let method = SignInMethod::from_word(text("method")?)?;
    let client_id = match text("clientId") {
        Some(id) => Some(ClientId::parse(id).ok()?),
        None => None,
    };
    if method.uses_device_flow() && client_id.is_none() {
        return None;
    }
    Some(GitHubAccount {
        key,
        method,
        client_id,
        scopes: shown("scopes"),
        token_expires: shown("tokenExpires"),
    })
}

fn account_value(account: &GitHubAccount) -> Value {
    let mut fields = BTreeMap::new();
    let mut put = |name: &str, value: &str| {
        fields.insert(name.to_owned(), Value::String(value.to_owned()));
    };
    put("host", &account.key.host().to_string());
    put("login", account.key.login());
    put("method", account.method.word());
    if let Some(id) = &account.client_id {
        put("clientId", id.as_str());
    }
    if let Some(scopes) = &account.scopes {
        put("scopes", scopes);
    }
    if let Some(expires) = &account.token_expires {
        put("tokenExpires", expires);
    }
    Value::Dict(fields)
}

/// The accounts and per-repository choices, in app settings.
#[derive(Clone)]
pub struct GitHubAccounts {
    settings: SharedSettings,
}

impl std::fmt::Debug for GitHubAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubAccounts").finish_non_exhaustive()
    }
}

impl GitHubAccounts {
    /// The accounts kept in `settings`.
    #[must_use]
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    /// Every account, sorted by host then login.
    #[must_use]
    pub fn load(&self) -> Vec<GitHubAccount> {
        let Some(Value::Array(items)) = self.settings.value(keys::GITHUB_ACCOUNTS) else {
            return Vec::new();
        };
        let mut accounts: Vec<GitHubAccount> = Vec::new();
        for item in &items {
            if let Some(account) = account_from(item)
                && !accounts.iter().any(|known| known.key == account.key)
            {
                accounts.push(account);
            }
        }
        accounts.sort_by(|a, b| a.key.cmp(&b.key));
        accounts
    }

    /// The account with `key`.
    #[must_use]
    pub fn find(&self, key: &AccountKey) -> Option<GitHubAccount> {
        self.load().into_iter().find(|account| account.key == *key)
    }

    /// Saves `account`, replacing the record with its key.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::GITHUB_ACCOUNTS`] when the list could not be saved.
    pub fn save(&self, account: &GitHubAccount) -> Result<(), SettingsError> {
        let mut accounts = self.load();
        accounts.retain(|known| known.key != account.key);
        accounts.push(account.clone());
        self.write(&accounts)
    }

    /// Forgets the account with `key` and every repository choice naming it.
    ///
    /// # Errors
    /// [`SettingsError`] naming the key that could not be saved.
    pub fn remove(&self, key: &AccountKey) -> Result<(), SettingsError> {
        let mut accounts = self.load();
        accounts.retain(|known| known.key != *key);
        self.write(&accounts)?;
        let mut choices = self.choices();
        choices.retain(|_, chosen| *chosen != key.id());
        self.write_choices(&choices)
    }

    fn write(&self, accounts: &[GitHubAccount]) -> Result<(), SettingsError> {
        let items = accounts.iter().map(account_value).collect();
        self.settings
            .set_value(keys::GITHUB_ACCOUNTS, Some(Value::Array(items)))
    }

    /// Repository path to the id ([`AccountKey::id`]) of the account chosen for it.
    #[must_use]
    pub fn choices(&self) -> BTreeMap<String, String> {
        self.settings
            .value(keys::GITHUB_REPOSITORY_ACCOUNTS)
            .and_then(|value| value.as_string_map())
            .unwrap_or_default()
    }

    /// Uses `key` for the repository at `repo_path` from now on.
    ///
    /// # Errors
    /// [`SettingsError`] naming [`keys::GITHUB_REPOSITORY_ACCOUNTS`] when it could not be saved.
    pub fn choose(&self, repo_path: &str, key: &AccountKey) -> Result<(), SettingsError> {
        let mut choices = self.choices();
        choices.insert(repo_path.to_owned(), key.id());
        self.write_choices(&choices)
    }

    fn write_choices(&self, choices: &BTreeMap<String, String>) -> Result<(), SettingsError> {
        self.settings.set_value(
            keys::GITHUB_REPOSITORY_ACCOUNTS,
            Some(Value::string_map(choices)),
        )
    }
}

/// Which account a repository uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChoice {
    /// This one.
    Use(GitHubAccount),
    /// Several accounts are signed into the host and none was chosen for the repository: ask
    /// which, and remember the answer with [`GitHubAccounts::choose`].
    Ask(Vec<GitHubAccount>),
    /// No account is signed into the host.
    NoAccount {
        /// The host the repository is on.
        host: GitHubHost,
    },
}

/// The account the repository at `repo_path`, on `host`, uses: the one account on that exact
/// host, or the one chosen for the repository among several, else [`AccountChoice::Ask`]. A
/// choice naming an account that is gone or on another host counts as no choice.
#[must_use]
pub fn account_for(
    host: &GitHubHost,
    repo_path: &str,
    accounts: &[GitHubAccount],
    choices: &BTreeMap<String, String>,
) -> AccountChoice {
    let on_host: Vec<&GitHubAccount> = accounts
        .iter()
        .filter(|account| account.key.host() == host)
        .collect();
    match on_host.as_slice() {
        [] => AccountChoice::NoAccount { host: host.clone() },
        [only] => AccountChoice::Use((*only).clone()),
        several => {
            let chosen = choices.get(repo_path);
            let picked = several
                .iter()
                .find(|account| Some(&account.key.id()) == chosen);
            match picked {
                Some(account) => AccountChoice::Use((*account).clone()),
                None => AccountChoice::Ask(several.iter().map(|a| (*a).clone()).collect()),
            }
        }
    }
}

/// The account for the repository at `repo_path` whose remote is `remote_url`. The remote URL
/// is untrusted: it only names the host, compared exactly with each account's.
///
/// # Errors
/// [`GitHubError::ForeignRemote`] when the URL names no GitHub repository on a plain host.
pub fn account_for_remote(
    remote_url: &str,
    repo_path: &str,
    accounts: &[GitHubAccount],
    choices: &BTreeMap<String, String>,
) -> Result<AccountChoice, GitHubError> {
    let host = remote_host(remote_url)?;
    Ok(account_for(&host, repo_path, accounts, choices))
}
