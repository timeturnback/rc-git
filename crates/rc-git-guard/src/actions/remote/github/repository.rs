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
    Upstream, current_branch, detached_upstream, redacted, resolve_upstream,
};
use crate::actions::{ActionError, ActionResult, open};

/// A repository's pull request remote. Its `Debug` and [`PullRequestRemote::shown`] leave
/// out any userinfo the URL carries.
#[derive(Clone, PartialEq, Eq)]
pub struct PullRequestRemote {
    name: String,
    url: String,
    shown: String,
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
}

impl fmt::Debug for PullRequestRemote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PullRequestRemote")
            .field("name", &self.name)
            .field("url", &self.shown)
            .finish()
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
    let upstream = match current_branch(&repo, OP) {
        Ok((branch, _)) => match resolve_upstream(&repo, &branch, Direction::Fetch, OP)? {
            Some(Upstream::Local { .. }) | None => detached_upstream(&repo, OP)?,
            named => named,
        },
        Err(ActionError::DetachedHead { .. }) => detached_upstream(&repo, OP)?,
        Err(other) => return Err(other),
    };
    Ok(upstream.and_then(|upstream| {
        let url = upstream.url()?;
        Some(PullRequestRemote {
            name: upstream.label(),
            url: url.to_bstring().to_string(),
            shown: redacted(url),
        })
    }))
}
