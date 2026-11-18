//! Which remote a repository's pull requests are read from: the one fetch and pull use. The
//! checked-out branch's `branch.<name>.remote`, else `origin`; `origin` too for a detached
//! `HEAD` and for a branch whose upstream is another local branch. Read from the repository's
//! own config with the isolated open, like every remote action, and through the same
//! resolvers (`configured_remote_name` and its `origin` fallback). The URL is untrusted: it
//! only ever names an owner and a repository through [`super::RepositoryTarget::for_account`].

use std::fmt;
use std::path::Path;

use gix::remote::Direction;

use crate::actions::remote::{
    Upstream, current_branch, detached_upstream, fetched_into, host_of, redacted, resolve_upstream,
};
use crate::actions::{ActionError, ActionResult, open};

/// A repository's pull request remote. Its `Debug` and [`PullRequestRemote::shown`] leave
/// out any userinfo the URL carries.
#[derive(Clone, PartialEq, Eq)]
pub struct PullRequestRemote {
    name: String,
    url: String,
    shown: String,
    host: String,
    uses_ssh: bool,
}

impl PullRequestRemote {
    /// The remote's name, e.g. `origin`; the URL itself when the branch names a bare URL.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The URL as configured, for [`super::RepositoryTarget::for_account`], which refuses one
    /// that carries userinfo. Never show it: use [`PullRequestRemote::shown`].
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The URL without userinfo, for messages.
    #[must_use]
    pub fn shown(&self) -> &str {
        &self.shown
    }

    /// The URL's host, with its port when it names one: the key git credentials and approved
    /// ssh host keys are stored under, as a fetch of this remote offers them.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the URL is ssh, whose secret is a key passphrase rather than an HTTPS token.
    #[must_use]
    pub fn uses_ssh(&self) -> bool {
        self.uses_ssh
    }
}

impl fmt::Debug for PullRequestRemote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PullRequestRemote")
            .field("name", &self.name)
            .field("url", &self.shown)
            .field("host", &self.host)
            .field("uses_ssh", &self.uses_ssh)
            .finish_non_exhaustive()
    }
}

/// The remote the repository at `repo_path` reads its pull requests from, `None` when it has
/// none with a URL.
///
/// # Errors
/// [`ActionError::Git`] when the repository or its remote configuration cannot be read.
pub fn pull_request_remote(repo_path: &Path) -> ActionResult<Option<PullRequestRemote>> {
    const OP: &str = "read the pull request remote";
    let repo = open(repo_path, OP, "repository")?;
    let upstream = pull_request_upstream(&repo, OP)?;
    Ok(upstream.and_then(|upstream| {
        let url = upstream.url()?;
        Some(PullRequestRemote {
            name: upstream.label(),
            url: url.to_bstring().to_string(),
            shown: redacted(url),
            host: host_of(url),
            uses_ssh: url.scheme == gix::url::Scheme::Ssh,
        })
    }))
}

/// The upstream pull requests are read from and fetched from, resolved once for both:
/// the checked-out branch's remote, else `origin`. Never [`Upstream::Local`]: a branch whose
/// upstream is another local branch reads `origin` too.
///
/// # Errors
/// [`ActionError::Git`] when `HEAD` or the remote configuration cannot be read.
pub(in crate::actions::remote) fn pull_request_upstream(
    repo: &gix::Repository,
    op: &'static str,
) -> ActionResult<Option<Upstream>> {
    match current_branch(repo, op) {
        Ok((branch, _)) => match resolve_upstream(repo, &branch, Direction::Fetch, op)? {
            Some(Upstream::Local { .. }) | None => detached_upstream(repo, op),
            named => Ok(named),
        },
        Err(ActionError::DetachedHead { .. }) => detached_upstream(repo, op),
        Err(other) => Err(other),
    }
}

/// Where this repository keeps its copy of the pull request remote's branch `branch` (a
/// pull request's base): the remote-tracking ref the remote's fetch refspecs file
/// `refs/heads/<branch>` under, e.g. `refs/remotes/origin/main`. Local only. `None` when the
/// remote is not a named one, when `branch` is not a valid branch name (it came from the
/// server), or when no refspec maps it; the ref itself may not exist yet.
///
/// # Errors
/// [`ActionError::Git`] when the repository or its remote configuration cannot be read.
pub fn pull_request_base_ref(repo_path: &Path, branch: &str) -> ActionResult<Option<String>> {
    const OP: &str = "read the pull request base";
    let repo = open(repo_path, OP, "repository")?;
    let Some(Upstream::Named { name, .. }) = pull_request_upstream(&repo, OP)? else {
        return Ok(None);
    };
    let full = format!("refs/heads/{branch}");
    if gix::refs::FullName::try_from(full.as_str()).is_err() {
        return Ok(None);
    }
    fetched_into(&repo, &name, &full, OP)
}
