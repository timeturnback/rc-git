//! Remote operations over HTTPS and SSH, and the plumbing they share: upstream
//! resolution, URL vetting, and credential injection. Both transports are in-process —
//! HTTPS through gix's curl backend, SSH through russh (`ssh/`) — so no child process
//! is ever spawned. Credentials always come from the caller; gix's helper cascade
//! (which spawns `git credential-*`) is never consulted.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{ActionError, ActionResult, open};

mod fetch;
mod pack;
mod pull;
mod push;
mod report;
mod ssh;

pub use fetch::{FetchOutcome, RefUpdate, fetch_upstream};
pub use pull::{PullOutcome, pull_fast_forward};
pub use push::{PushOutcome, push_upstream};

/// How long dialing, the TLS or ssh handshake, and authentication may take together.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a transfer may move no bytes before it counts as dead. A git server sends
/// keepalives every few seconds while it thinks, so this much total silence means the
/// connection is gone rather than the server being slow on a large repository.
pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a blocked read or write wakes to look at the cancel flag. It bounds how
/// long a cancel takes to land; the wake costs one timer while the socket is silent
/// and nothing at all while bytes are arriving.
pub(super) const CANCEL_POLL: Duration = Duration::from_millis(200);

/// A shared "stop" flag for one remote operation, cloneable so the transport can hold
/// it after the call that started the transfer has returned. Setting it from another
/// thread ends the operation with [`ActionError::Cancelled`]: the ssh transport wakes
/// out of its read within a fifth of a second, and the pack steps of both transports
/// check it between objects.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// A flag nobody has set yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the operation holding this flag to stop. Safe from any thread.
    pub fn signal(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`Cancel::signal`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// The raw flag, for the gix APIs that take `&AtomicBool` as their interrupt.
    pub(super) fn flag(&self) -> &AtomicBool {
        &self.0
    }

    /// A handle the ssh transport keeps for as long as the connection lives.
    pub(super) fn share(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.0)
    }
}

/// What one sync pass did: fetch, fast-forward, and — when the branch was ahead — push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    /// The fetch that ran first.
    pub fetch: FetchOutcome,
    /// The fast-forward that followed.
    pub pull: PullOutcome,
    /// The push, `None` when the branch had nothing to push.
    pub push: Option<PushOutcome>,
}

/// Fetch, fast-forward the checked-out branch, then push it when it is ahead.
/// Stops at the first failure; a divergence surfaces as [`ActionError::Diverged`]
/// from the pull step before anything is uploaded.
///
/// # Errors
/// Everything [`fetch_upstream`], [`pull_fast_forward`], and [`push_upstream`] report.
pub fn sync(
    repo_path: &Path,
    policy: &crate::Policy,
    auth: RemoteAuth<'_>,
    cancel: &Cancel,
) -> ActionResult<SyncOutcome> {
    let fetch = fetch_upstream(repo_path, policy, auth, cancel)?;
    let pull = pull_fast_forward(repo_path, policy)?;
    let status = upstream_status(repo_path)?;
    let push = if status.ahead > 0 {
        Some(push_upstream(repo_path, policy, auth, cancel)?)
    } else {
        None
    };
    Ok(SyncOutcome { fetch, pull, push })
}

/// Everything a remote operation may need to authenticate, borrowed from the caller.
/// For HTTPS remotes only `creds` matters; for SSH remotes `creds.token` doubles as the
/// key passphrase (`username` is ignored) and `accepted_hostkey` is the SHA256
/// fingerprint the user approved for an otherwise unknown host.
#[derive(Debug, Clone, Copy, Default)]
pub struct RemoteAuth<'a> {
    /// HTTPS username/token, or SSH key passphrase in `token`.
    pub creds: Option<&'a Credentials>,
    /// A host-key fingerprint (`SHA256:...`) the user chose to trust.
    pub accepted_hostkey: Option<&'a str>,
    /// Where `config`, `known_hosts`, and default keys live; `~/.ssh` when `None`.
    /// Tests point it at a fixture directory.
    pub ssh_dir: Option<&'a std::path::Path>,
}

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
/// Without `branch.<name>.merge` — a branch made with `git init` and pushed without
/// `-u` — it is `refs/remotes/<remote>/<branch>` on the remote `configured_remote_name`
/// picks: the ref push uploads to and moves, so ahead/behind and pull agree with push.
fn tracking_ref_name(repo: &gix::Repository, branch: &str) -> Option<String> {
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().ok()?;
    let Some(tracking) =
        repo.branch_remote_tracking_ref_name(full_name.as_ref(), gix::remote::Direction::Fetch)
    else {
        let remote = configured_remote_name(repo, branch)?;
        return Some(format!("refs/remotes/{remote}/{branch}"));
    };
    Some(tracking.ok()?.as_bstr().to_string())
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

/// The transport family a vetted remote URL belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteScheme {
    /// `https://`, or plain `http://` to a loopback host so tests can run a local
    /// server end to end.
    Https,
    /// `ssh://` or an scp-like `git@host:path` URL.
    Ssh,
}

/// Classifies the URL, refusing every scheme rc-git will not talk to (`git://`,
/// `file://`, non-loopback `http://`).
fn vet_remote_url(
    url: &gix::Url,
    op: &'static str,
    remote_name: &str,
) -> ActionResult<RemoteScheme> {
    match url.scheme {
        gix::url::Scheme::Https => Ok(RemoteScheme::Https),
        gix::url::Scheme::Http if is_loopback(url) => Ok(RemoteScheme::Https),
        gix::url::Scheme::Ssh => Ok(RemoteScheme::Ssh),
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
fn bail_if_cancelled(cancel: &Cancel, op: &'static str) -> ActionResult<()> {
    if cancel.is_cancelled() {
        return Err(ActionError::Cancelled { op });
    }
    Ok(())
}

/// Transport timeouts for the HTTPS backend. A repository opened with
/// `Options::isolated()` reads no global configuration, so `http.lowSpeedLimit` and
/// friends can only come from here; without them curl waits forever on a peer that
/// accepts the connection and then goes quiet.
///
/// `low_speed_limit_bytes_per_second` is 1 rather than a realistic rate on purpose:
/// the pair means "fail when fewer than 1 byte moves for [`IDLE_TIMEOUT`]", which
/// never punishes a slow link, only a dead one.
fn http_timeouts(options: &mut gix::protocol::transport::client::blocking_io::http::Options) {
    options.connect_timeout = Some(CONNECT_TIMEOUT);
    options.low_speed_limit_bytes_per_second = 1;
    options.low_speed_time_seconds = IDLE_TIMEOUT.as_secs();
}

/// Whether `ancestor` is reachable from `tip`.
fn is_ancestor(repo: &gix::Repository, ancestor: gix::ObjectId, tip: gix::ObjectId) -> bool {
    if ancestor == tip {
        return true;
    }
    repo.merge_base(ancestor, tip)
        .is_ok_and(|base| base.detach() == ancestor)
}

/// Gives the repository an in-memory `user.name`/`user.email` so reflog lines can be
/// written under the isolated open. The configuration file on disk is never touched.
fn set_reflog_identity(repo: &mut gix::Repository, op: &'static str) -> ActionResult<()> {
    let mut config = repo.config_snapshot_mut();
    let mut set = |key: &'static dyn gix::config::tree::Key, value: &str| -> ActionResult<()> {
        config
            .set_value(key, value)
            .map_err(|source| ActionError::Git {
                op,
                name: "configuration".to_owned(),
                source: Box::new(source),
            })?;
        Ok(())
    };
    set(&gix::config::tree::User::NAME, "rc-git")?;
    set(&gix::config::tree::User::EMAIL, "rc-git@localhost")?;
    config.commit().map_err(|source| ActionError::Git {
        op,
        name: "configuration".to_owned(),
        source: Box::new(source),
    })?;
    Ok(())
}
