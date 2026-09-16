//! Remote operations over HTTPS, and the plumbing they share: upstream resolution,
//! URL vetting, and credential injection. rc-git speaks HTTPS only — every other
//! transport would spawn a process — and credentials always come from the caller,
//! so gix's helper cascade (which spawns `git credential-*`) is never consulted.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{ActionError, ActionResult, open};

mod fetch;

pub use fetch::{FetchOutcome, RefUpdate, fetch_upstream};

/// A username and token for one HTTPS host. Never stored and never logged by this crate.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The username, e.g. `git` for GitHub token auth.
    pub username: String,
    /// The token or password.
    pub token: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Where the checked-out branch pushes and pulls, and how far apart the sides are.
/// `ahead`/`behind` compare the branch against its remote-tracking ref as of the last
/// fetch; both are 0 when the tracking ref does not exist yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamStatus {
    /// Short name of the checked-out branch.
    pub branch: String,
    /// Upstream remote name, e.g. `origin`. Empty when `has_upstream` is false.
    pub remote: String,
    /// Full name of the remote-tracking ref, e.g. `refs/remotes/origin/main`.
    pub upstream_ref: String,
    /// Fetch URL of the remote, userinfo removed.
    pub url: String,
    /// Host (with port when present) parsed from the URL — the credential key.
    pub host: String,
    /// Commits on the branch that the tracking ref does not have.
    pub ahead: usize,
    /// Commits on the tracking ref that the branch does not have.
    pub behind: usize,
    /// Whether the branch has a configured upstream at all.
    pub has_upstream: bool,
}

/// The checked-out branch's upstream configuration and ahead/behind counts. Local only:
/// no network I/O, no credentials.
///
/// # Errors
/// [`ActionError::DetachedHead`] when `HEAD` is not on a branch, [`ActionError::Git`]
/// when the repository cannot be read.
pub fn upstream_status(repo_path: &Path) -> ActionResult<UpstreamStatus> {
    const OP: &str = "read upstream status";
    let repo = open(repo_path, OP, "repository")?;
    let (branch, head_id) = current_branch(&repo, OP)?;
    let no_upstream = |branch: &str| UpstreamStatus {
        branch: branch.to_owned(),
        remote: String::new(),
        upstream_ref: String::new(),
        url: String::new(),
        host: String::new(),
        ahead: 0,
        behind: 0,
        has_upstream: false,
    };
    let Some(remote_name) = configured_remote_name(&repo, &branch) else {
        return Ok(no_upstream(&branch));
    };
    let Some(url) = remote_fetch_url(&repo, &remote_name, OP)? else {
        return Ok(no_upstream(&branch));
    };
    let upstream_ref = tracking_ref_name(&repo, &branch);
    let (ahead, behind) = match (&upstream_ref, head_id) {
        (Some(upstream_ref), Some(head_id)) => match peel_ref(&repo, upstream_ref) {
            Some(upstream_id) => count_ahead_behind(&repo, head_id, upstream_id, OP)?,
            None => (0, 0),
        },
        (Some(_) | None, Some(_) | None) => (0, 0),
    };
    Ok(UpstreamStatus {
        branch,
        remote: remote_name,
        upstream_ref: upstream_ref.unwrap_or_default(),
        host: host_of(&url),
        url: redacted(&url),
        ahead,
        behind,
        has_upstream: true,
    })
}

/// The checked-out branch's short name and tip, `tip` being `None` on an unborn branch.
fn current_branch(
    repo: &gix::Repository,
    op: &'static str,
) -> ActionResult<(String, Option<gix::ObjectId>)> {
    let head_name = repo
        .head_name()
        .map_err(|source| ActionError::Git {
            op,
            name: "HEAD".to_owned(),
            source: Box::new(source),
        })?
        .ok_or(ActionError::DetachedHead { op })?;
    let branch = head_name.shorten().to_string();
    let tip = repo.head_id().ok().map(gix::Id::detach);
    Ok((branch, tip))
}

/// `branch.<name>.remote` from the repository's own configuration, falling back to
/// `origin` when that remote exists.
fn configured_remote_name(repo: &gix::Repository, branch: &str) -> Option<String> {
    use gix::bstr::ByteSlice;
    if let Some(name) =
        repo.branch_remote_name(branch.as_bytes().as_bstr(), gix::remote::Direction::Fetch)
    {
        return Some(name.as_bstr().to_string());
    }
    repo.remote_names()
        .iter()
        .any(|name| name.as_slice() == b"origin")
        .then(|| "origin".to_owned())
}

/// The remote's fetch URL, or `None` when the remote has no URL configured.
fn remote_fetch_url(
    repo: &gix::Repository,
    remote_name: &str,
    op: &'static str,
) -> ActionResult<Option<gix::Url>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let remote = repo
        .find_remote(remote_name)
        .map_err(|source| ActionError::Git {
            op,
            name: remote_name.to_owned(),
            source: Box::new(source),
        })?;
    Ok(remote.url(gix::remote::Direction::Fetch).cloned())
}

/// Full name of the branch's remote-tracking ref, e.g. `refs/remotes/origin/main`.
fn tracking_ref_name(repo: &gix::Repository, branch: &str) -> Option<String> {
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().ok()?;
    let tracking = repo
        .branch_remote_tracking_ref_name(full_name.as_ref(), gix::remote::Direction::Fetch)?
        .ok()?;
    Some(tracking.as_bstr().to_string())
}

/// Peels `name` to a commit id, `None` when the ref does not exist.
fn peel_ref(repo: &gix::Repository, name: &str) -> Option<gix::ObjectId> {
    let reference = repo.find_reference(name).ok()?;
    reference.into_fully_peeled_id().ok().map(gix::Id::detach)
}

/// Commits reachable from `local` but not `upstream`, and the reverse.
fn count_ahead_behind(
    repo: &gix::Repository,
    local: gix::ObjectId,
    upstream: gix::ObjectId,
    op: &'static str,
) -> ActionResult<(usize, usize)> {
    if local == upstream {
        return Ok((0, 0));
    }
    let count = |tip: gix::ObjectId, hide: gix::ObjectId| -> ActionResult<usize> {
        let walk = repo
            .rev_walk([tip])
            .with_hidden([hide])
            .all()
            .map_err(|source| ActionError::Git {
                op,
                name: tip.to_string(),
                source: Box::new(source),
            })?;
        Ok(walk.flatten().count())
    };
    Ok((count(local, upstream)?, count(upstream, local)?))
}

/// Refuses every URL rc-git will not talk to: anything but `https://`, except plain
/// `http://` to a loopback host so tests can run a local server end to end.
fn require_https(url: &gix::Url, op: &'static str, remote_name: &str) -> ActionResult<()> {
    match url.scheme {
        gix::url::Scheme::Https => Ok(()),
        gix::url::Scheme::Http if is_loopback(url) => Ok(()),
        ref scheme => Err(ActionError::UnsupportedRemote {
            op,
            name: remote_name.to_owned(),
            scheme: scheme.as_str().to_owned(),
        }),
    }
}

fn is_loopback(url: &gix::Url) -> bool {
    matches!(
        url.host(),
        Some("127.0.0.1" | "::1" | "[::1]" | "localhost")
    )
}

/// `host` or `host:port`, the key credentials are stored under.
fn host_of(url: &gix::Url) -> String {
    let host = url.host().unwrap_or_default();
    match url.port {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// The URL without userinfo, safe for error messages.
fn redacted(url: &gix::Url) -> String {
    let mut url = url.clone();
    url.user = None;
    url.password = None;
    url.to_bstring().to_string()
}

/// The credential callback handed to gix: answers with the caller's credentials, or
/// fails so the operation reports [`ActionError::AuthRequired`]. gix invokes it only
/// after the server answered 401, so anonymous access to public remotes works.
#[expect(
    clippy::result_large_err,
    reason = "the closure's signature is fixed by gix::remote::Connection::with_credentials"
)]
fn authenticate_with(
    creds: Option<Credentials>,
) -> impl FnMut(gix::credentials::helper::Action) -> gix::credentials::protocol::Result {
    move |action| match action {
        gix::credentials::helper::Action::Get(context) => match creds.clone() {
            Some(creds) => Ok(Some(gix::credentials::protocol::Outcome {
                identity: gix::sec::identity::Account {
                    username: creds.username,
                    password: creds.token,
                    oauth_refresh_token: None,
                },
                next: context.into(),
            })),
            None => Err(gix::credentials::protocol::Error::IdentityMissing { context }),
        },
        gix::credentials::helper::Action::Store(_) | gix::credentials::helper::Action::Erase(_) => {
            Ok(None)
        }
    }
}

/// Maps a network failure to [`ActionError::AuthRequired`] when the chain shows the
/// server demanded credentials, else wraps it as [`ActionError::Git`].
fn map_network_error(
    error: Box<dyn std::error::Error + Send + Sync + 'static>,
    op: &'static str,
    name: &str,
    host: &str,
) -> ActionError {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error.as_ref());
    while let Some(err) = current {
        if let Some(io) = err.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return ActionError::AuthRequired {
                op,
                host: host.to_owned(),
            };
        }
        let text = err.to_string();
        if text.contains("401") || text.contains("Could not obtain identity") {
            return ActionError::AuthRequired {
                op,
                host: host.to_owned(),
            };
        }
        current = err.source();
    }
    ActionError::Git {
        op,
        name: name.to_owned(),
        source: error,
    }
}

/// Returns [`ActionError::Cancelled`] when the flag is already set, so a cancelled
/// operation never opens a connection.
fn bail_if_cancelled(cancel: &AtomicBool, op: &'static str) -> ActionResult<()> {
    if cancel.load(Ordering::SeqCst) {
        return Err(ActionError::Cancelled { op });
    }
    Ok(())
}
