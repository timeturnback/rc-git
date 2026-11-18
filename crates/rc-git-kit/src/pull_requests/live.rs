//! The live [`PullRequestService`]: the guard's reads, the app's account settings, and the
//! token keeper over the keychain. The app and the CLI both use it, so they share the accounts,
//! the per-repository choices and the tokens.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rc_git_guard::actions::github::{
    ApiToken, list_open_pull_requests, pull_request_remote, read_pull_request, refresh_user_token,
    unix_now,
};

use super::link::{Linked, RepositoryLink, link_repository};
use super::{Cancel, GitHubError, PageCursor, PullRequestDetail, PullRequestPage};
use super::{PullRequestService, ReadError};
use crate::github::accounts::{GitHubAccount, GitHubAccounts};
use crate::github::tokens::{TokenError, TokenKeeper};
use crate::platform::api_tokens::{AccountKey, ApiTokenStore};
use crate::platform::stores::SharedSettings;

/// The service over the guard, the settings and the token store.
pub struct LivePullRequests {
    accounts: GitHubAccounts,
    keeper: TokenKeeper<Arc<dyn ApiTokenStore>>,
    ssh_dir: Option<PathBuf>,
}

impl std::fmt::Debug for LivePullRequests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LivePullRequests")
            .field("ssh_dir", &self.ssh_dir)
            .finish_non_exhaustive()
    }
}

impl LivePullRequests {
    /// The service over the accounts in `settings` and the tokens in `tokens`, reading ssh
    /// aliases from `~/.ssh/config`.
    #[must_use]
    pub fn new(settings: SharedSettings, tokens: Arc<dyn ApiTokenStore>) -> Self {
        Self {
            accounts: GitHubAccounts::new(settings),
            keeper: TokenKeeper::new(tokens),
            ssh_dir: None,
        }
    }

    /// The same, reading ssh aliases from `dir/config` instead (the CLI's `--ssh-dir`, tests).
    #[must_use]
    pub fn with_ssh_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.ssh_dir = dir;
        self
    }

    /// [`PullRequestService::link`], with account `login` chosen when several are signed in on
    /// the remote's host: the CLI's `--login`, an explicit choice for one command that is not
    /// saved. A login that is not among them chooses nothing, and the link still asks.
    #[must_use]
    pub fn link_choosing(&self, repo_path: &str, login: &str) -> RepositoryLink {
        let link = self.link(repo_path);
        let RepositoryLink::ChooseAccount { accounts, .. } = &link else {
            return link;
        };
        let Some(key) = accounts.iter().find(|key| key.login() == login) else {
            return link;
        };
        let Ok(remote) = pull_request_remote(Path::new(repo_path)) else {
            return RepositoryLink::Hidden;
        };
        let mut choices = self.accounts.choices();
        choices.insert(repo_path.to_owned(), key.id());
        link_repository(
            remote.as_ref(),
            repo_path,
            &self.accounts.load(),
            &choices,
            self.ssh_dir.as_deref(),
        )
    }

    /// The account's working token, renewed when it expired.
    fn token(&self, account: &GitHubAccount, cancel: &Cancel) -> Result<ApiToken, ReadError> {
        let host = account.key.host();
        let fresh = self
            .keeper
            .token(account, unix_now(), |refresh| match &account.client_id {
                Some(client) => refresh_user_token(host, client, refresh, cancel),
                None => Err(GitHubError::RefreshRejected {
                    host: host.to_string(),
                    code: "no client id".to_owned(),
                }),
            });
        match fresh {
            Ok(fresh) => Ok(fresh.token),
            Err(TokenError::SignedOut { account, .. }) => Err(ReadError::SignedOut { account }),
            Err(TokenError::Store(error)) => Err(ReadError::Failed(error.to_string())),
            Err(TokenError::GitHub(error)) => Err(ReadError::from_github(&error)),
        }
    }
}

impl PullRequestService for LivePullRequests {
    fn link(&self, repo_path: &str) -> RepositoryLink {
        let Ok(remote) = pull_request_remote(Path::new(repo_path)) else {
            return RepositoryLink::Hidden;
        };
        link_repository(
            remote.as_ref(),
            repo_path,
            &self.accounts.load(),
            &self.accounts.choices(),
            self.ssh_dir.as_deref(),
        )
    }

    fn list(
        &self,
        linked: &Linked,
        after: Option<&PageCursor>,
        cancel: &Cancel,
    ) -> Result<PullRequestPage, ReadError> {
        let token = self.token(&linked.account, cancel)?;
        let host = linked.account.key.host();
        list_open_pull_requests(host, &token, &linked.target, after, cancel)
            .map_err(|error| ReadError::from_github(&error))
    }

    fn detail(
        &self,
        linked: &Linked,
        number: u64,
        cancel: &Cancel,
    ) -> Result<PullRequestDetail, ReadError> {
        let token = self.token(&linked.account, cancel)?;
        let host = linked.account.key.host();
        read_pull_request(host, &token, &linked.target, number, cancel)
            .map_err(|error| ReadError::from_github(&error))
    }

    fn choose_account(&self, repo_path: &str, key: &AccountKey) -> Result<(), String> {
        self.accounts
            .choose(repo_path, key)
            .map_err(|error| error.to_string())
    }
}
