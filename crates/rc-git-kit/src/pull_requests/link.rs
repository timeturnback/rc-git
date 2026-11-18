//! Which GitHub repository, and through which account, a repository's pull requests are read
//! from. The repository's remote URL is untrusted: it names a host to look an account up by
//! and an owner and a name, nothing else. The account follows [`account_for`]'s rule (one on
//! the host: that one; several: the one chosen for this repository, else ask), and the token
//! then goes only to that account's host, through a [`RepositoryTarget`] on exactly it.
//!
//! An ssh remote whose host is an alias in the user's ssh config (`git@github-work:o/r`)
//! names the alias, which matches no account. When no account is on the written host, the
//! alias's `HostName`, resolved by the ssh transport's own reader, is looked up instead; the
//! alias alone never picks an account.

use std::path::Path;

use rc_git_guard::actions::github::{
    GitHubHost, PullRequestRemote, RepositoryTarget, remote_host, ssh_dialed_host,
};

use crate::github::accounts::{AccountChoice, GitHubAccount, account_for};
use crate::platform::api_tokens::AccountKey;
use std::collections::BTreeMap;

/// A repository linked to its GitHub repository and the account that reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    /// The account whose token reads it.
    pub account: GitHubAccount,
    /// The repository, on the account's host.
    pub target: RepositoryTarget,
    /// The remote it came from, e.g. `origin`.
    pub remote: String,
}

/// What a repository's remote says about its pull requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryLink {
    /// No remote, or one that names no repository on a host with an account (other than
    /// github.com): the Pull Requests section stays hidden.
    Hidden,
    /// The remote is on github.com and no account is signed in there: the section offers the
    /// sign-in.
    SignIn {
        /// The host to sign in to.
        host: GitHubHost,
    },
    /// Several accounts are signed in on the host and none was chosen for this repository:
    /// the section asks which.
    ChooseAccount {
        /// The host.
        host: GitHubHost,
        /// The accounts to choose from.
        accounts: Vec<AccountKey>,
    },
    /// Linked: the section lists the pull requests.
    Linked(Linked),
}

/// The link for the repository at `repo_path` whose pull request remote is `remote`, given
/// the signed-in `accounts`, the per-repository `choices`, and the user's ssh config in
/// `ssh_dir` (`None`: `~/.ssh`).
#[must_use]
pub fn link_repository(
    remote: Option<&PullRequestRemote>,
    repo_path: &str,
    accounts: &[GitHubAccount],
    choices: &BTreeMap<String, String>,
    ssh_dir: Option<&Path>,
) -> RepositoryLink {
    let Some(remote) = remote else {
        return RepositoryLink::Hidden;
    };
    let Ok(written) = remote_host(remote.url()) else {
        return RepositoryLink::Hidden;
    };
    let mut choice = account_for(&written, repo_path, accounts, choices);
    if matches!(choice, AccountChoice::NoAccount { .. })
        && let Ok(Some(dialed)) = ssh_dialed_host(remote.url(), ssh_dir)
    {
        choice = account_for(&dialed, repo_path, accounts, choices);
    }
    match choice {
        AccountChoice::Use(account) => {
            let host = account.key.host().clone();
            match RepositoryTarget::for_account(&host, remote.url(), ssh_dir) {
                Ok(target) => RepositoryLink::Linked(Linked {
                    account,
                    target,
                    remote: remote.name().to_owned(),
                }),
                Err(_) => RepositoryLink::Hidden,
            }
        }
        AccountChoice::Ask(several) => RepositoryLink::ChooseAccount {
            host: several
                .first()
                .map_or(written, |account| account.key.host().clone()),
            accounts: several.into_iter().map(|account| account.key).collect(),
        },
        AccountChoice::NoAccount { host } if host.is_github_com() => {
            RepositoryLink::SignIn { host }
        }
        AccountChoice::NoAccount { .. } => RepositoryLink::Hidden,
    }
}
