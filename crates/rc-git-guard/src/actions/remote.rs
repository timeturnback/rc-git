//! Remote operations over HTTPS and SSH, and the plumbing they share: upstream
//! resolution, URL vetting, and credential injection. Both transports are in-process —
//! HTTPS through gix's curl backend, SSH through russh (`ssh/`) — so no child process
//! is ever spawned. Credentials always come from the caller; gix's helper cascade
//! (which spawns `git credential-*`) is never consulted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use gix::remote::Direction;

use super::{ActionError, ActionResult, git_err, open};

mod fetch;
pub mod github;
mod pack;
mod pull;
mod pull_head;
mod push;
mod report;
mod ssh;

pub use fetch::{FetchOutcome, FetchRemote, RefUpdate, fetch_remote, fetch_upstream};
pub use pull::{PullOutcome, pull_fast_forward};
pub use pull_head::{
    PULL_HEAD_NAMESPACE, PullHeadFetch, PullHeadRequest, fetch_pull_request_head, pull_head_ref,
};
pub use push::{PushOutcome, push_upstream};

/// How long dialing, the TLS or ssh handshake, and authentication may take together.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a transfer may move no bytes before it counts as dead. A git server sends
/// keepalives every few seconds while it thinks (`upload-pack` always; `receive-pack` only
/// over `side-band-64k`, which push requests for this), so this much total silence means
/// the connection is gone rather than the server being slow on a large repository.
pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a blocked read or write wakes to look at the cancel flag. It bounds how
/// long a cancel takes to land; the wake costs one timer while the socket is silent
/// and nothing at all while bytes are arriving.
pub(super) const CANCEL_POLL: Duration = Duration::from_millis(200);

/// A shared "stop" flag for one remote operation, cloneable so the transport can hold
/// it after the call that started the transfer has returned. Setting it from another
/// thread ends the operation with [`ActionError::Cancelled`] within a fifth of a second:
/// the ssh transport wakes out of its read to look at it, and every HTTPS network step
/// runs on a detached thread the caller stops waiting for. The pack steps of both
/// transports also check it between objects.
///
/// It also carries the idle budget both transports measure silence against,
/// two minutes unless [`Cancel::with_idle_timeout`] changes it, and the connect budget
/// dialing, the handshake and authentication share, 30 s unless
/// [`Cancel::with_connect_timeout`] changes it. A read that blocks in
/// curl never sees the flag, so after a cancel the abandoned HTTPS thread lingers until
/// its next object, chunk or this budget (the connect budget while still connecting) ends it.
/// A fetch keeps its repository claimed until that thread ends, so the next
/// fetch of the same repository waits for it instead of racing its ref updates.
#[derive(Debug, Clone)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    idle: Duration,
    connect: Duration,
}

impl Default for Cancel {
    fn default() -> Self {
        Self {
            flag: Arc::default(),
            idle: IDLE_TIMEOUT,
            connect: CONNECT_TIMEOUT,
        }
    }
}

impl Cancel {
    /// A flag nobody has set yet, with the default idle budget.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same flag with a different idle budget: how long a transfer may move no
    /// bytes before it fails. HTTPS counts it in whole seconds, rounding down, with a
    /// floor of one.
    #[must_use]
    pub fn with_idle_timeout(self, idle: Duration) -> Self {
        Self { idle, ..self }
    }

    /// The same flag with a different connect budget: how long dialing, the TLS or ssh
    /// handshake, and authentication may take together before the operation fails.
    #[must_use]
    pub fn with_connect_timeout(self, connect: Duration) -> Self {
        Self { connect, ..self }
    }

    /// Asks the operation holding this flag to stop. Safe from any thread.
    pub fn signal(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether [`Cancel::signal`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// The raw flag, for the gix APIs that take `&AtomicBool` as their interrupt.
    pub(super) fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    /// A shared handle to the flag, for whoever sets it from outside the operation: the
    /// ssh transport keeps one for as long as the connection lives, and a signal handler
    /// stores `true` into one to cancel.
    #[must_use]
    pub fn share(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// How long a transfer may move no bytes before it counts as dead.
    pub(super) fn idle(&self) -> Duration {
        self.idle
    }

    /// How long dialing, the handshake, and authentication may take together.
    pub(super) fn connect(&self) -> Duration {
        self.connect
    }
}

/// Everything a remote operation may need to authenticate, borrowed from the caller.
/// For HTTPS remotes only `creds` matters; for SSH remotes `creds.token` doubles as the
/// key passphrase (`username` is ignored) and `accepted_hostkey` is the SHA256
/// fingerprint the user approved for an otherwise unknown host.
#[derive(Debug, Clone, Copy, Default)]
pub struct RemoteAuth<'a> {
    /// HTTPS username/token, or SSH key passphrase in `token`.
    pub creds: Option<&'a Credentials>,
    /// The host (`host` or `host:port`) `creds` were issued for. An HTTPS server's 401 gets
    /// them only when it comes from this host; when `None`, only when it comes from the host
    /// the remote URL names.
    pub host: Option<&'a str>,
    /// A host-key fingerprint (`SHA256:...`) the user chose to trust.
    pub accepted_hostkey: Option<&'a str>,
    /// Where `config`, `known_hosts`, and default keys live; `~/.ssh` when `None`.
    /// Tests point it at a fixture directory.
    pub ssh_dir: Option<&'a std::path::Path>,
}

/// An HTTPS username and token, or (token only) an SSH key passphrase. Never stored and never
/// logged by this crate.
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
/// fetch; both are 0 when the tracking ref does not exist yet, which `published` tells apart
/// from a branch that is in step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamStatus {
    /// Short name of the checked-out branch.
    pub branch: String,
    /// Upstream remote name, e.g. `origin`; `.` when the upstream is a local branch, and the
    /// URL without userinfo when `branch.<name>.remote` holds one. Empty when `has_upstream`
    /// is false.
    pub remote: String,
    /// Full name of the remote-tracking ref, e.g. `refs/remotes/origin/main`, or of the local
    /// branch a local upstream names. Empty for a URL upstream, which has no tracking ref.
    pub upstream_ref: String,
    /// Fetch URL of the remote, userinfo removed. Empty for a local upstream.
    pub url: String,
    /// Host (with port when present) parsed from the URL — the credential key.
    pub host: String,
    /// Commits on the branch that the tracking ref does not have.
    pub ahead: usize,
    /// Commits on the tracking ref that the branch does not have.
    pub behind: usize,
    /// Whether an upstream was resolved for the branch: a local branch, a URL, or a remote
    /// with a fetch URL from `branch.<name>.remote`, else `origin` when it exists. True even
    /// before a tracking ref exists; `ahead` and `behind` are then 0.
    pub has_upstream: bool,
    /// Whether the tracking ref (or the local branch a local upstream names) exists, so
    /// `ahead` and `behind` were counted. False for a never-pushed branch, a URL upstream,
    /// and without an upstream.
    pub published: bool,
    /// Whether the remote URL parses as ssh (`ssh://` or scp-like `user@host:path`), the
    /// transport that takes a key passphrase rather than an HTTPS token. False without an
    /// upstream. Read from the parsed scheme, since `url` drops the user an scp-like
    /// display form would need.
    pub uses_ssh: bool,
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
    let Some(upstream) = resolve_upstream(&repo, &branch, Direction::Fetch, OP)? else {
        return Ok(UpstreamStatus {
            branch,
            remote: String::new(),
            upstream_ref: String::new(),
            url: String::new(),
            host: String::new(),
            ahead: 0,
            behind: 0,
            has_upstream: false,
            published: false,
            uses_ssh: false,
        });
    };
    let upstream_ref = tracking_ref_name(&repo, &branch, OP)?.and_then(|tracking| tracking.name);
    let upstream_id = upstream_ref
        .as_deref()
        .and_then(|name| peel_ref(&repo, name));
    let (ahead, behind) = match (upstream_id, head_id) {
        (Some(upstream_id), Some(head_id)) => count_ahead_behind(&repo, head_id, upstream_id, OP)?,
        (Some(_) | None, Some(_) | None) => (0, 0),
    };
    let url = upstream.url();
    Ok(UpstreamStatus {
        branch,
        remote: upstream.label(),
        upstream_ref: upstream_ref.unwrap_or_default(),
        host: url.map(host_of).unwrap_or_default(),
        uses_ssh: url.is_some_and(|url| url.scheme == gix::url::Scheme::Ssh),
        url: url.map(redacted).unwrap_or_default(),
        ahead,
        behind,
        has_upstream: true,
        published: upstream_id.is_some(),
    })
}

/// The checked-out branch's short name and tip, `tip` being `None` on an unborn branch.
fn current_branch(
    repo: &gix::Repository,
    op: &'static str,
) -> ActionResult<(String, Option<gix::ObjectId>)> {
    let head_name = repo
        .head_name()
        .map_err(git_err(op, "HEAD"))?
        .ok_or(ActionError::DetachedHead { op })?;
    let branch = head_name.shorten().to_string();
    let tip = super::head_commit(repo, op)?;
    Ok((branch, tip))
}

/// The branch's remote for `direction` from the repository's own configuration, falling
/// back to `origin` when that remote exists. Fetch reads `branch.<name>.remote`; push reads
/// `branch.<name>.pushRemote`, then `remote.pushDefault`, then `branch.<name>.remote`, as git
/// does.
fn configured_remote_name(
    repo: &gix::Repository,
    branch: &str,
    direction: Direction,
) -> Option<String> {
    use gix::bstr::ByteSlice;
    #[expect(
        clippy::disallowed_methods,
        reason = "the one sanctioned resolver; it adds the origin fallback"
    )]
    let configured = repo.branch_remote_name(branch.as_bytes().as_bstr(), direction);
    if let Some(name) = configured {
        return Some(name.as_bstr().to_string());
    }
    default_remote_name(repo)
}

/// The remote used when no branch names one: `origin`, when that remote exists.
fn default_remote_name(repo: &gix::Repository) -> Option<String> {
    repo.remote_names()
        .iter()
        .any(|name| name.as_slice() == b"origin")
        .then(|| "origin".to_owned())
}

/// What `branch.<name>.remote` says when the upstream is another branch of this repository.
const LOCAL_REMOTE: &str = ".";

/// Where the checked-out branch's upstream lives, resolved once by [`configured_remote_name`]
/// for status, fetch, pull and push alike: the fetch remote for the first three, the push
/// remote for push.
pub(super) enum Upstream {
    /// `branch.<name>.remote = .`: another branch of this repository, e.g. `refs/heads/main`.
    Local {
        /// Full name of the local branch `branch.<name>.merge` names.
        merge_ref: String,
    },
    /// A configured remote and its URL for the direction resolved.
    Named {
        /// The remote's name, e.g. `origin`.
        name: String,
        /// The remote's URL: `remote.<name>.url` for fetch, `remote.<name>.pushurl` falling
        /// back to it for push.
        url: gix::Url,
    },
    /// A URL written straight into `branch.<name>.remote`, with no remote configured for it.
    Url {
        /// The URL as configured.
        url: gix::Url,
    },
}

/// The refusal fetch and pull give a branch whose `branch.<name>.remote` is a bare URL: with no
/// remote there are no fetch refspecs and no remote-tracking ref, so rc-git reads only from a
/// named remote and tells the user to add one. Push still works (see `CONCEPTS.md`).
fn url_upstream_refused(op: &'static str, url: &gix::Url) -> ActionError {
    ActionError::git(
        op,
        redacted(url),
        "the upstream is a URL with no remote configured for it; rc-git fetches and pulls \
         only from a named remote (add one with `git remote add <name> <url>` and point \
         the branch's remote at it)",
    )
}

impl Upstream {
    /// What reports and errors call the upstream: `.`, the remote's name, or the URL
    /// without userinfo.
    pub(super) fn label(&self) -> String {
        match self {
            Self::Local { .. } => LOCAL_REMOTE.to_owned(),
            Self::Named { name, .. } => name.clone(),
            Self::Url { url } => redacted(url),
        }
    }

    /// The URL a transfer talks to, `None` for a local upstream.
    pub(super) fn url(&self) -> Option<&gix::Url> {
        match self {
            Self::Local { .. } => None,
            Self::Named { url, .. } | Self::Url { url } => Some(url),
        }
    }
}

/// Resolves the branch's upstream. `None` when no remote is configured, the named remote
/// has no URL, or a local upstream names no branch (`branch.<name>.merge` unset).
fn resolve_upstream(
    repo: &gix::Repository,
    branch: &str,
    direction: Direction,
    op: &'static str,
) -> ActionResult<Option<Upstream>> {
    let Some(name) = configured_remote_name(repo, branch, direction) else {
        return Ok(None);
    };
    if name == LOCAL_REMOTE {
        return Ok(merge_ref_name(repo, branch).map(|merge_ref| Upstream::Local { merge_ref }));
    }
    if is_named_remote(repo, &name) {
        let url = find_named_remote(repo, &name, op)?.url(direction).cloned();
        return Ok(url.map(|url| Upstream::Named { name, url }));
    }
    let url = gix::url::parse(name.as_str()).map_err(|_unparsable| {
        ActionError::git(
            op,
            format!("branch.{branch}.remote"),
            "the value is neither a configured remote nor a URL",
        )
    })?;
    Ok(Some(Upstream::Url { url }))
}

/// The upstream a detached `HEAD` fetches from: [`default_remote_name`] and its fetch URL, as
/// `git fetch` with no branch to consult. `None` when there is no `origin` or it has no URL.
fn detached_upstream(repo: &gix::Repository, op: &'static str) -> ActionResult<Option<Upstream>> {
    let Some(name) = default_remote_name(repo) else {
        return Ok(None);
    };
    let url = find_named_remote(repo, &name, op)?
        .url(Direction::Fetch)
        .cloned();
    Ok(url.map(|url| Upstream::Named { name, url }))
}

/// Whether `name` is a remote's name rather than a URL: a configured remote, or a word
/// with no `/` (git's own test), so a misspelt name still fails as a missing remote.
fn is_named_remote(repo: &gix::Repository, name: &str) -> bool {
    !name.contains('/')
        || repo
            .remote_names()
            .iter()
            .any(|remote| remote.as_slice() == name.as_bytes())
}

/// `branch.<name>.merge` as a full ref name, e.g. `refs/heads/main`.
fn merge_ref_name(repo: &gix::Repository, branch: &str) -> Option<String> {
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().ok()?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the one sanctioned resolver; a local upstream has no remote to fall back from"
    )]
    let merge = repo.branch_remote_ref_name(full_name.as_ref(), Direction::Fetch)?;
    Some(merge.ok()?.as_bstr().to_string())
}

/// The configured remote named `remote_name`, read from the repository's own configuration.
fn find_named_remote<'repo>(
    repo: &'repo gix::Repository,
    remote_name: &str,
    op: &'static str,
) -> ActionResult<gix::Remote<'repo>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let remote = repo
        .find_remote(remote_name)
        .map_err(git_err(op, remote_name))?;
    Ok(remote)
}

/// A branch's remote-tracking ref and where its name came from.
struct TrackingRef {
    /// Full name, e.g. `refs/remotes/origin/main`. `None` when the branch names no upstream
    /// and the remote's fetch refspecs do not map it (a `--single-branch` clone): no fetch
    /// ever writes such a ref, so there is nothing to count against.
    name: Option<String>,
    /// Whether `branch.<name>.merge` named it. False for the `origin` fallback, so a
    /// missing ref then means the branch was never published rather than deleted upstream.
    configured: bool,
}

/// The branch's remote-tracking ref, e.g. `refs/remotes/origin/main`, or the local branch
/// itself (`refs/heads/main`) for a local upstream; `None` for a URL upstream, which has
/// no tracking ref. Without `branch.<name>.merge` — a branch made with `git init` and
/// pushed without `-u` — it is where the fetch refspecs of the remote
/// `configured_remote_name` picks map `refs/heads/<branch>` ([`fetched_into`]), the ref a
/// push to that remote moves. A push also refuses a `branch.<name>.merge` that names another
/// branch ([`push_refs`]), so ahead/behind and pull agree with push.
///
/// # Errors
/// [`ActionError::Git`] when the branch name, its upstream configuration or the fallback
/// remote cannot be read.
fn tracking_ref_name(
    repo: &gix::Repository,
    branch: &str,
    op: &'static str,
) -> ActionResult<Option<TrackingRef>> {
    let Some(remote) = configured_remote_name(repo, branch, Direction::Fetch) else {
        return Ok(None);
    };
    if remote == LOCAL_REMOTE {
        return Ok(merge_ref_name(repo, branch).map(|name| TrackingRef {
            name: Some(name),
            configured: true,
        }));
    }
    let full = format!("refs/heads/{branch}");
    let full_name: gix::refs::FullName = full.as_str().try_into().map_err(git_err(op, &full))?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the one sanctioned resolver; it adds the origin fallback"
    )]
    let configured = repo.branch_remote_tracking_ref_name(full_name.as_ref(), Direction::Fetch);
    let Some(tracking) = configured else {
        if !is_named_remote(repo, &remote) {
            return Ok(None);
        }
        return Ok(Some(TrackingRef {
            name: fetched_into(repo, &remote, &full, op)?,
            configured: false,
        }));
    };
    let name = tracking.map_err(git_err(op, &full))?;
    Ok(Some(TrackingRef {
        name: Some(name.as_bstr().to_string()),
        configured: true,
    }))
}

/// The branch a push of `branch` writes on the remote and the remote-tracking ref it moves:
/// where the push remote's fetch refspecs file that branch, as git moves it after a push
/// (`None` for a URL upstream, which has none). Same name only, as git's
/// `push.default=simple`: when the push goes to the remote the branch pulls from and
/// `branch.<name>.merge` names another branch, status and pull count against that one, so a
/// same-name push would never clear them. That case is refused, and rc-git never creates a
/// differently named remote branch. A push to another remote (`pushRemote`, `pushDefault`)
/// skips that check, as git's `simple` does.
pub(super) fn push_refs(
    repo: &gix::Repository,
    branch: &str,
    upstream: &Upstream,
    op: &'static str,
) -> ActionResult<(String, Option<String>)> {
    let full_ref = format!("refs/heads/{branch}");
    let pushes_where_it_pulls = configured_remote_name(repo, branch, Direction::Push)
        == configured_remote_name(repo, branch, Direction::Fetch);
    if pushes_where_it_pulls
        && let Some(merge_ref) = merge_ref_name(repo, branch)
        && merge_ref != full_ref
    {
        let label = upstream.label();
        let short = merge_ref.strip_prefix("refs/heads/").unwrap_or(&merge_ref);
        return Err(ActionError::git(
            op,
            branch,
            format!(
                "upstream {label}/{short} does not match branch {branch}; rc-git pushes only \
                 to the same-named branch, so push this one with git"
            ),
        ));
    }
    let tracking = match upstream {
        Upstream::Named { name, .. } => fetched_into(repo, name, &full_ref, op)?,
        Upstream::Local { .. } | Upstream::Url { .. } => None,
    };
    Ok((full_ref, tracking))
}

/// Where `remote_name`'s fetch refspecs file its branch `remote_ref`, e.g.
/// `refs/remotes/fork/main` for `refs/heads/main`; `None` when no refspec maps it.
fn fetched_into(
    repo: &gix::Repository,
    remote_name: &str,
    remote_ref: &str,
    op: &'static str,
) -> ActionResult<Option<String>> {
    use gix::bstr::ByteSlice;
    let remote = find_named_remote(repo, remote_name, op)?;
    let group = gix::refspec::MatchGroup {
        specs: remote
            .refspecs(Direction::Fetch)
            .iter()
            .map(gix::refspec::RefSpec::to_ref)
            .filter(|spec| spec.source().is_some() && spec.destination().is_some())
            .collect(),
    };
    let null = repo.object_hash().null();
    let item = gix::refspec::match_group::Item {
        full_ref_name: remote_ref.as_bytes().as_bstr(),
        target: &null,
        object: None,
    };
    let matched = group.match_lhs(std::iter::once(item));
    Ok(matched
        .mappings
        .into_iter()
        .find_map(|mapping| mapping.rhs)
        .map(|rhs| rhs.to_string()))
}

/// Peels `name` to a commit id, `None` when the ref does not exist.
fn peel_ref(repo: &gix::Repository, name: &str) -> Option<gix::ObjectId> {
    let reference = repo.find_reference(name).ok()?;
    reference.into_fully_peeled_id().ok().map(gix::Id::detach)
}

/// Commits reachable from `local` but not `upstream`, and the reverse. The pair of ids
/// fully determines the answer, so it is served from [`AHEAD_BEHIND`] when the same pair
/// was counted before; a shallow repository always walks, since deepening it changes the
/// counts without moving either id.
fn count_ahead_behind(
    repo: &gix::Repository,
    local: gix::ObjectId,
    upstream: gix::ObjectId,
    op: &'static str,
) -> ActionResult<(usize, usize)> {
    if local == upstream {
        return Ok((0, 0));
    }
    let walk_both = || -> ActionResult<(usize, usize)> {
        let count = |tip: gix::ObjectId, hide: gix::ObjectId| -> ActionResult<usize> {
            let walk = repo
                .rev_walk([tip])
                .with_hidden([hide])
                .all()
                .map_err(|source| ActionError::git(op, tip.to_string(), source))?;
            Ok(walk.flatten().count())
        };
        Ok((count(local, upstream)?, count(upstream, local)?))
    };
    if repo.is_shallow() {
        return walk_both();
    }
    AHEAD_BEHIND.get_or_count(local, upstream, walk_both)
}

/// Process-wide memo for [`count_ahead_behind`]. The preflight runs on every ref event and
/// tab switch while the two tips rarely move, so a handful of entries covers every open
/// window.
static AHEAD_BEHIND: AheadBehindMemo = AheadBehindMemo::new();

/// How many (local, upstream) pairs [`AheadBehindMemo`] keeps.
const AHEAD_BEHIND_ENTRIES: usize = 8;

/// A bounded most-recently-used cache from a (local, upstream) commit pair to its
/// (ahead, behind) counts.
struct AheadBehindMemo {
    entries: std::sync::Mutex<Vec<AheadBehindEntry>>,
}

/// One remembered (local, upstream) pair and its (ahead, behind) counts.
type AheadBehindEntry = ((gix::ObjectId, gix::ObjectId), (usize, usize));

impl AheadBehindMemo {
    const fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The counts for the pair, from the memo or from `count`, which runs without the lock
    /// held so a slow walk never blocks another repository's lookup. A failed count is
    /// not remembered.
    fn get_or_count(
        &self,
        local: gix::ObjectId,
        upstream: gix::ObjectId,
        count: impl FnOnce() -> ActionResult<(usize, usize)>,
    ) -> ActionResult<(usize, usize)> {
        let key = (local, upstream);
        if let Some(&(_, counts)) = self.lock().iter().find(|(entry, _)| *entry == key) {
            return Ok(counts);
        }
        let counts = count()?;
        let mut entries = self.lock();
        entries.retain(|(entry, _)| *entry != key);
        entries.insert(0, (key, counts));
        entries.truncate(AHEAD_BEHIND_ENTRIES);
        Ok(counts)
    }

    /// The entries, recovered from a poisoned lock: they are plain values written in one
    /// step each, so a panic elsewhere cannot leave one half-written.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<AheadBehindEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
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
    matches!(url.host(), Some("127.0.0.1" | "[::1]" | "localhost"))
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
/// fails so the operation reports [`ActionError::AuthRequired`] (or
/// [`ActionError::CredentialHostRefused`] when the request names a host other than the
/// remote's own). gix invokes it only
/// after the server answered 401, so anonymous access to public remotes works. The
/// credentials go only to a 401 whose context names `host` (`host` or `host:port`, as
/// [`host_of`] spells it): a request to any other host, or one naming no host, gets none.
#[expect(
    clippy::result_large_err,
    reason = "the closure's signature is fixed by gix::remote::Connection::with_credentials"
)]
fn authenticate_with(
    creds: Option<Credentials>,
    host: String,
) -> impl FnMut(gix::credentials::helper::Action) -> gix::credentials::protocol::Result {
    move |action| match action {
        gix::credentials::helper::Action::Get(context) => match creds.clone() {
            Some(creds) if context_host(&context).as_deref() == Some(host.as_str()) => {
                Ok(Some(gix::credentials::protocol::Outcome {
                    identity: gix::sec::identity::Account {
                        username: creds.username,
                        password: creds.token,
                        oauth_refresh_token: None,
                    },
                    next: context.into(),
                }))
            }
            Some(_) | None => Err(gix::credentials::protocol::Error::IdentityMissing { context }),
        },
        gix::credentials::helper::Action::Store(_) | gix::credentials::helper::Action::Erase(_) => {
            Ok(None)
        }
    }
}

/// The host a credential request is for, spelled as [`host_of`] spells it: parsed from the
/// request's URL, else its `host` field. `None` when it names neither.
fn context_host(context: &gix::credentials::protocol::Context) -> Option<String> {
    if let Some(url) = &context.url {
        return gix::url::parse(url.as_slice())
            .ok()
            .map(|url| host_of(&url));
    }
    context.host.clone()
}

/// Maps a network failure to [`ActionError::CredentialHostRefused`] when a host other than
/// `host` asked for credentials (the remote redirected there), to
/// [`ActionError::AuthRequired`] when the server demanded credentials, else wraps it as
/// [`ActionError::Git`]. Only the handshake's credential errors, a transport-level 401 and a
/// missing identity count; a local I/O failure deeper in the chain (an unwritable pack
/// directory) stays a [`ActionError::Git`]. An ssh server's refusal replaces gix's wrapping
/// as the cause, so its stderr is what shows.
fn map_network_error(
    error: Box<dyn std::error::Error + Send + Sync + 'static>,
    op: &'static str,
    name: &str,
    host: &str,
) -> ActionError {
    if let Some(requested) = foreign_credential_host(error.as_ref(), host) {
        return ActionError::CredentialHostRefused {
            op,
            host: host.to_owned(),
            requested,
        };
    }
    if demands_credentials(error.as_ref()) {
        return ActionError::AuthRequired {
            op,
            host: host.to_owned(),
        };
    }
    if let Some(refusal) = ssh::ServerRefusal::find(error.as_ref()) {
        return ActionError::git(op, name, refusal.clone());
    }
    ActionError::git(op, name, error)
}

/// Whether `error` is the server asking for credentials, or rejecting the ones sent.
fn demands_credentials(error: &(dyn std::error::Error + 'static)) -> bool {
    handshake_error(error).is_some_and(rejected_credentials)
        || transport_io_error(error)
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
        || lacks_identity(error)
}

/// The handshake error at the top of `error`: push's `advertise` returns it as is, and
/// fetch's `prepare_fetch` wraps it in two `transparent` variants `source()` skips.
fn handshake_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a gix::protocol::handshake::Error> {
    if let Some(handshake) = error.downcast_ref::<gix::protocol::handshake::Error>() {
        return Some(handshake);
    }
    let prepare = error.downcast_ref::<gix::remote::fetch::prepare::Error>()?;
    if let gix::remote::fetch::prepare::Error::RefMap(gix::remote::ref_map::Error::Handshake(
        handshake,
    )) = prepare
    {
        return Some(handshake);
    }
    None
}

/// Whether the handshake failed on credentials: none offered, none returned, or refused.
fn rejected_credentials(error: &gix::protocol::handshake::Error) -> bool {
    use gix::protocol::handshake::Error;
    match error {
        Error::Credentials(_) | Error::EmptyCredentials | Error::InvalidCredentials { .. } => true,
        Error::Transport(_)
        | Error::TransportProtocolPolicyViolation { .. }
        | Error::ParseRefs(_) => false,
    }
}

/// The transport's own I/O error after the handshake, where gix reports a 401 as
/// `PermissionDenied`: push's bare transport error or its request error, and fetch's
/// client error. An I/O error under the pack write or the ref updates is not one.
fn transport_io_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a std::io::Error> {
    use gix::protocol::transport::client;
    if let Some(io) = error.downcast_ref::<std::io::Error>() {
        return Some(io);
    }
    let mut client_error = error.downcast_ref::<client::Error>();
    if let Some(
        gix::remote::fetch::Error::Client(inner)
        | gix::remote::fetch::Error::Fetch(gix::protocol::fetch::Error::Client(inner)),
    ) = error.downcast_ref::<gix::remote::fetch::Error>()
    {
        client_error = Some(inner);
    }
    if let Some(client::Error::Io(io)) = client_error {
        return Some(io);
    }
    None
}

/// Whether the credential callback said it has no identity to offer, anywhere in the chain.
fn lacks_identity(error: &(dyn std::error::Error + 'static)) -> bool {
    missing_identity_context(error).is_some()
}

/// The request the credential callback refused for want of an identity, anywhere in the
/// chain.
fn missing_identity_context<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a gix::credentials::protocol::Context> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(gix::credentials::protocol::Error::IdentityMissing { context }) =
            err.downcast_ref::<gix::credentials::protocol::Error>()
        {
            return Some(context);
        }
        current = err.source();
    }
    None
}

/// The host that asked for credentials, when it is not `host`: the transport followed a
/// redirect and the 401 came from there. [`authenticate_with`] never answers such a
/// request, so asking the user again cannot help. `None` when the request named `host`, or
/// no host at all.
fn foreign_credential_host(
    error: &(dyn std::error::Error + 'static),
    host: &str,
) -> Option<String> {
    let requested = context_host(missing_identity_context(error)?)?;
    (requested != host).then_some(requested)
}

/// Returns [`ActionError::Cancelled`] when the flag is already set, so a cancelled
/// operation never opens a connection.
fn bail_if_cancelled(cancel: &Cancel, op: &'static str) -> ActionResult<()> {
    if cancel.is_cancelled() {
        return Err(ActionError::Cancelled { op });
    }
    Ok(())
}

/// Runs one blocking transfer step on a detached thread and waits for it, looking at
/// `cancel` every [`CANCEL_POLL`]. curl has no interrupt, so a read it is blocked in never
/// sees the flag; waiting here instead lets a cancel return [`ActionError::Cancelled`]
/// within a fifth of a second while the abandoned thread runs on until its next cancel
/// check or curl's own connect or idle timeout ends it. The thread is never joined: a
/// scoped thread would make the cancel wait for curl after all. A result that arrives
/// before the cancel is noticed wins, since the step it reports has already happened.
fn run_detached<T, F>(cancel: &Cancel, op: &'static str, work: F) -> ActionResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ActionResult<T> + Send + 'static,
{
    use std::sync::mpsc::{RecvTimeoutError, channel};
    let (done, finished) = channel();
    std::thread::Builder::new()
        .name(format!("rc-git {op}"))
        .spawn(move || {
            // The receiver is gone only when the caller already returned Cancelled, so
            // nobody is left to hand the result to.
            drop(done.send(work()));
        })
        .map_err(|source| ActionError::git(op, "transfer thread", source))?;
    loop {
        match finished.recv_timeout(CANCEL_POLL) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) => bail_if_cancelled(cancel, op)?,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ActionError::git(
                    op,
                    "transfer thread",
                    "the transfer thread ended without a result",
                ));
            }
        }
    }
}

/// Common git directories of the repositories with a transfer in flight, abandoned
/// HTTPS threads included.
static IN_FLIGHT: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Signalled whenever a [`TransferClaim`] is released.
static RELEASED: Condvar = Condvar::new();

/// The in-flight set. A panic cannot leave a set insert or remove half-done, so a
/// poisoned lock still holds a consistent set and is used as is.
fn in_flight() -> MutexGuard<'static, BTreeSet<PathBuf>> {
    IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One repository's right to run a transfer that writes its pack and refs. It moves onto
/// the thread doing the transfer and is released when that thread drops it, so a thread
/// [`run_detached`] abandoned keeps the repository claimed until it really ends.
#[derive(Debug)]
pub(super) struct TransferClaim {
    dir: PathBuf,
}

impl Drop for TransferClaim {
    fn drop(&mut self) {
        in_flight().remove(&self.dir);
        RELEASED.notify_all();
    }
}

/// Claims `repo` for one transfer. While another transfer of the same repository still
/// runs — typically a cancelled HTTPS fetch whose thread waits on curl — this waits for it,
/// looking at `cancel` every [`CANCEL_POLL`], for at most the connect budget.
///
/// # Errors
/// [`ActionError::Cancelled`] when `cancel` is set while waiting, [`ActionError::Git`]
/// when the repository directory cannot be resolved or the other transfer is still running
/// once the connect budget is spent.
pub(super) fn claim_transfer(
    repo: &gix::Repository,
    cancel: &Cancel,
    op: &'static str,
) -> ActionResult<TransferClaim> {
    let common = repo.common_dir();
    let dir = common
        .canonicalize()
        .map_err(git_err(op, &common.display().to_string()))?;
    let deadline = Instant::now() + cancel.connect();
    let mut held = in_flight();
    while held.contains(&dir) {
        bail_if_cancelled(cancel, op)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ActionError::git(
                op,
                dir.display().to_string(),
                "another transfer of this repository is still running (a cancelled one ends at \
                 its next object or its idle timeout); retry in a moment",
            ));
        }
        held = RELEASED
            .wait_timeout(held, left.min(CANCEL_POLL))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    held.insert(dir.clone());
    Ok(TransferClaim { dir })
}

/// The HTTP transport options every fetch and push uses, built from code-owned defaults.
/// Nothing comes from the repository: its `http.*`, `remote.<name>.proxy` and
/// `credential.*` keys are never read, so a repository someone else produced cannot route
/// a transfer through its own proxy, switch off certificate checks, or spawn a credential
/// helper through `proxy_authenticate`. Callers must always install these options; a gix
/// connection left without any loads them from the repository's configuration.
///
/// libcurl still honours `http_proxy`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` from the
/// process environment, as git does. A loopback URL sets an empty proxy, which tells
/// libcurl to use none and ignore those variables, so local test servers stay hermetic.
fn http_options(
    url: &gix::Url,
    cancel: &Cancel,
) -> gix::protocol::transport::client::blocking_io::http::Options {
    let mut options = gix::protocol::transport::client::blocking_io::http::Options::default();
    http_timeouts(&mut options, cancel.connect(), cancel.idle());
    if is_loopback(url) {
        options.proxy = Some(String::new());
    }
    options
}

/// Transport timeouts for the HTTPS backend. [`http_options`] reads no repository
/// configuration, so `http.lowSpeedLimit` and friends can only come from here; without
/// them curl waits forever on a peer that accepts the connection and then goes quiet.
///
/// `low_speed_limit_bytes_per_second` is 1 rather than a realistic rate on purpose:
/// the pair means "fail when fewer than 1 byte moves for `idle`", which never punishes
/// a slow link, only a dead one. curl counts that time in whole seconds, and 0 would
/// switch the check off, so a budget under a second becomes one second.
fn http_timeouts(
    options: &mut gix::protocol::transport::client::blocking_io::http::Options,
    connect: Duration,
    idle: Duration,
) {
    options.connect_timeout = Some(connect);
    options.low_speed_limit_bytes_per_second = 1;
    options.low_speed_time_seconds = idle.as_secs().max(1);
}

/// Gives the repository an in-memory `user.name`/`user.email` so reflog lines can be
/// written under the isolated open. The configuration file on disk is never touched.
fn set_reflog_identity(repo: &mut gix::Repository, op: &'static str) -> ActionResult<()> {
    let mut config = repo.config_snapshot_mut();
    let mut set = |key: &'static dyn gix::config::tree::Key, value: &str| -> ActionResult<()> {
        config
            .set_value(key, value)
            .map_err(git_err(op, "configuration"))?;
        Ok(())
    };
    set(&gix::config::tree::User::NAME, super::REFLOG_NAME)?;
    set(&gix::config::tree::User::EMAIL, super::REFLOG_EMAIL)?;
    config.commit().map_err(git_err(op, "configuration"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(byte: u8) -> gix::ObjectId {
        gix::ObjectId::from_bytes_or_panic(&[byte; 20])
    }

    #[test]
    fn ahead_behind_memo_walks_once_per_pair() {
        let memo = AheadBehindMemo::new();
        let walks = std::cell::Cell::new(0);
        let count = |counts: (usize, usize)| {
            walks.set(walks.get() + 1);
            Ok(counts)
        };
        let first = memo.get_or_count(oid(1), oid(2), || count((3, 20_000)));
        let again = memo.get_or_count(oid(1), oid(2), || count((0, 0)));
        assert_eq!(first.ok(), Some((3, 20_000)));
        assert_eq!(again.ok(), Some((3, 20_000)));
        assert_eq!(walks.get(), 1);

        let swapped = memo.get_or_count(oid(2), oid(1), || count((20_000, 3)));
        assert_eq!(swapped.ok(), Some((20_000, 3)));
        assert_eq!(walks.get(), 2);
    }

    #[test]
    fn ahead_behind_memo_forgets_a_failed_count_and_stays_bounded() {
        let memo = AheadBehindMemo::new();
        let walks = std::cell::Cell::new(0);
        let failed = memo.get_or_count(oid(1), oid(2), || {
            walks.set(walks.get() + 1);
            Err(ActionError::Cancelled { op: "count" })
        });
        assert!(failed.is_err());
        let retried = memo.get_or_count(oid(1), oid(2), || {
            walks.set(walks.get() + 1);
            Ok((1, 1))
        });
        assert_eq!(retried.ok(), Some((1, 1)));
        assert_eq!(walks.get(), 2);

        for byte in 10..10 + u8::try_from(AHEAD_BEHIND_ENTRIES).unwrap() {
            let filled = memo.get_or_count(oid(byte), oid(2), || Ok((0, 1)));
            assert_eq!(filled.ok(), Some((0, 1)));
        }
        assert_eq!(memo.lock().len(), AHEAD_BEHIND_ENTRIES);
        let evicted = memo.get_or_count(oid(1), oid(2), || {
            walks.set(walks.get() + 1);
            Ok((1, 1))
        });
        assert_eq!(evicted.ok(), Some((1, 1)));
        assert_eq!(walks.get(), 3);
    }

    #[test]
    fn credentials_debug_redacts_the_token_alone_and_inside_remote_auth() {
        let creds = Credentials {
            username: "u".into(),
            token: "secret-value".into(),
        };
        let auth = RemoteAuth {
            creds: Some(&creds),
            ..RemoteAuth::default()
        };
        for printed in [format!("{creds:?}"), format!("{auth:?}")] {
            assert!(!printed.contains("secret-value"), "{printed}");
            assert!(printed.contains("<redacted>"), "{printed}");
        }
    }

    /// What the credential callback answers to a 401 for `url`, holding credentials
    /// issued for `host`.
    fn answer(host: &str, url: &str) -> Option<String> {
        let creds = Credentials {
            username: "u".into(),
            token: "t".into(),
        };
        let mut callback = authenticate_with(Some(creds), host.to_owned());
        let action = gix::credentials::helper::Action::get_for_url(url);
        callback(action)
            .ok()
            .flatten()
            .map(|outcome| outcome.identity.password)
    }

    #[test]
    fn credentials_answer_only_a_401_from_their_own_host() {
        assert_eq!(
            answer("example.com", "https://example.com/repo.git"),
            Some("t".to_owned())
        );
        assert_eq!(
            answer("example.com:8443", "https://example.com:8443/repo.git"),
            Some("t".to_owned())
        );
        for (host, url) in [
            ("example.com", "https://other.example/repo.git"),
            ("example.com", "https://example.com:8443/repo.git"),
            ("example.com:8443", "https://example.com/repo.git"),
            ("", "https://example.com/repo.git"),
            ("example.com", "https://example.com.evil.test/repo.git"),
        ] {
            assert_eq!(answer(host, url), None, "{host} answered {url}");
        }
    }

    /// The error gix reports when the credential callback refused a request for `url`.
    fn identity_missing_for(url: &str) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        let context = gix::credentials::protocol::Context {
            url: Some(url.into()),
            ..Default::default()
        };
        Box::new(gix::protocol::handshake::Error::Credentials(
            gix::credentials::protocol::Error::IdentityMissing { context },
        ))
    }

    #[test]
    fn a_credential_request_from_another_host_is_a_refused_host() {
        let mapped = map(identity_missing_for("http://127.0.0.1:4011/repo.git"));
        assert!(
            matches!(
                &mapped,
                ActionError::CredentialHostRefused { op: "fetch", host, requested }
                    if host == "127.0.0.1:4010" && requested == "127.0.0.1:4011"
            ),
            "{mapped}"
        );
    }

    #[test]
    fn a_credential_request_from_the_remote_host_is_auth_required() {
        let mapped = map(identity_missing_for("http://127.0.0.1:4010/repo.git"));
        assert!(
            matches!(mapped, ActionError::AuthRequired { .. }),
            "{mapped}"
        );
        let nameless = gix::credentials::protocol::Error::IdentityMissing {
            context: gix::credentials::protocol::Context::default(),
        };
        let mapped = map(Box::new(nameless));
        assert!(
            matches!(mapped, ActionError::AuthRequired { .. }),
            "{mapped}"
        );
    }

    #[test]
    fn a_401_naming_no_host_gets_no_credentials() {
        let creds = Credentials {
            username: "u".into(),
            token: "t".into(),
        };
        let mut callback = authenticate_with(Some(creds), String::new());
        let context = gix::credentials::protocol::Context::default();
        let answer = callback(gix::credentials::helper::Action::Get(context));
        assert!(answer.is_err(), "{answer:?}");
    }

    fn map(error: Box<dyn std::error::Error + Send + Sync + 'static>) -> ActionError {
        map_network_error(error, "fetch", "origin", "127.0.0.1:4010")
    }

    #[test]
    fn a_connect_failure_whose_text_holds_401_is_a_git_error() {
        let error = std::io::Error::other("Failed to connect to 127.0.0.1 port 4010 after 1401 ms");
        let mapped = map(Box::new(error));
        assert!(matches!(mapped, ActionError::Git { .. }), "{mapped}");
    }

    #[test]
    fn a_transport_permission_denied_is_auth_required() {
        let error = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let mapped = map(Box::new(error));
        assert!(
            matches!(mapped, ActionError::AuthRequired { .. }),
            "{mapped}"
        );
    }

    #[test]
    fn a_permission_denied_while_writing_the_pack_is_a_git_error() {
        let local = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let error = gix::remote::fetch::Error::Fetch(gix::protocol::fetch::Error::ConsumePack(
            Box::new(local),
        ));
        let mapped = map(Box::new(error));
        assert!(matches!(mapped, ActionError::Git { .. }), "{mapped}");
    }

    #[test]
    fn a_401_after_the_handshake_is_auth_required() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let error = gix::remote::fetch::Error::Fetch(gix::protocol::fetch::Error::Client(
            gix::protocol::transport::client::Error::Io(denied),
        ));
        let mapped = map(Box::new(error));
        assert!(
            matches!(mapped, ActionError::AuthRequired { .. }),
            "{mapped}"
        );
    }

    #[test]
    fn rejected_credentials_in_the_fetch_handshake_are_auth_required() {
        let error =
            gix::remote::fetch::prepare::Error::RefMap(gix::remote::ref_map::Error::Handshake(
                gix::protocol::handshake::Error::EmptyCredentials,
            ));
        let mapped = map(Box::new(error));
        assert!(
            matches!(mapped, ActionError::AuthRequired { .. }),
            "{mapped}"
        );
    }

    fn parse_url(raw: &str) -> gix::Url {
        gix::url::parse(raw).unwrap()
    }

    #[test]
    fn is_loopback_sees_a_bracketed_ipv6_host_over_http() {
        let url = parse_url("http://[::1]:4010/x.git");
        assert_eq!(url.host(), Some("[::1]"));
        assert!(is_loopback(&url));
        assert!(matches!(
            vet_remote_url(&url, "fetch", "origin"),
            Ok(RemoteScheme::Https)
        ));
    }

    #[test]
    fn is_loopback_is_never_asked_about_an_ssh_ipv6_host() {
        let url = parse_url("ssh://[::1]/x.git");
        assert_eq!(url.host(), Some("::1"));
        assert!(matches!(
            vet_remote_url(&url, "fetch", "origin"),
            Ok(RemoteScheme::Ssh)
        ));
    }

    #[test]
    fn is_loopback_refuses_plain_http_to_another_host() {
        let url = parse_url("http://example.com/x.git");
        assert!(!is_loopback(&url));
        assert!(matches!(
            vet_remote_url(&url, "fetch", "origin"),
            Err(ActionError::UnsupportedRemote { .. })
        ));
    }
}
