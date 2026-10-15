//! Fetch from the checked-out branch's remote (or `origin` on a detached HEAD)
//! over HTTPS or SSH, updating remote-tracking refs exactly as `git fetch <remote>` would. gix
//! writes the received pack and the tracking refs itself; nothing else is touched, and
//! `FETCH_HEAD` is never written.

use std::path::Path;

use crate::actions::write::Writer;
use crate::actions::{ActionError, ActionResult, authorize, git_err, open};
use crate::{Action, Confirmation, Policy};

use super::{
    Cancel, Credentials, RemoteAuth, RemoteScheme, Upstream, authenticate_with, bail_if_cancelled,
    claim_transfer, detached_upstream, host_of, http_options, map_network_error, resolve_upstream,
    run_detached, url_upstream_refused, vet_remote_url,
};

const OP: &str = "fetch";

/// One ref (remote-tracking branch or tag) a fetch created or moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    /// Full ref name, e.g. `refs/remotes/origin/main`.
    pub name: String,
    /// The object it now points at: a commit, or a tag object for an annotated tag.
    pub target: gix::ObjectId,
}

/// What a fetch changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// The remote fetched from.
    pub remote: String,
    /// Refs (remote-tracking branches and tags) created or moved, empty when everything was up to
    /// date.
    pub updated_refs: Vec<RefUpdate>,
}

/// Fetches from the checked-out branch's remote (`origin` when none is configured, and
/// always `origin` on a detached `HEAD`), using the remote's own refspecs. `auth.creds` answer
/// an HTTPS server's 401 and, over SSH, `creds.token` unlocks an encrypted key. A local
/// upstream (`branch.<name>.remote = .`) has nothing to transfer, so the fetch is a no-op.
///
/// # Errors
/// [`ActionError::UnsupportedRemote`] for a remote scheme rc-git does not speak,
/// [`ActionError::AuthRequired`] when the server rejects the credentials,
/// [`ActionError::Cancelled`] when `cancel` is set, [`ActionError::NoUpstream`] when no
/// remote is configured, or, on a detached `HEAD` (reported as branch `HEAD`), when there is no
/// `origin`, [`ActionError::UnknownHostKey`] and [`ActionError::HostKeyMismatch`] when an SSH
/// host is unknown or its key changed, [`ActionError::PassphraseRequired`] when an encrypted
/// SSH key needs `creds.token`, [`ActionError::Git`] for every other failure, including an
/// upstream that is a bare URL rather than a named remote.
pub fn fetch_upstream(
    repo_path: &Path,
    policy: &Policy,
    auth: RemoteAuth<'_>,
    cancel: &Cancel,
) -> ActionResult<FetchOutcome> {
    let proof = authorize(policy, &Action::Fetch, OP, "remote", None)?;
    bail_if_cancelled(cancel, OP)?;
    let mut repo = open(repo_path, OP, "repository")?;
    super::set_reflog_identity(&mut repo, OP)?;
    let repo = repo;
    let upstream = fetch_source(&repo)?;
    let (remote_name, url) = match upstream {
        Upstream::Local { .. } => {
            return Ok(FetchOutcome {
                remote: upstream.label(),
                updated_refs: Vec::new(),
            });
        }
        Upstream::Url { url } => return Err(url_upstream_refused(OP, &url)),
        Upstream::Named { name, url } => (name, url),
    };
    let remote = find_remote(&repo, &remote_name)?;
    let scheme = vet_remote_url(&url, OP, &remote_name)?;
    let host = host_of(&url);
    let claim = claim_transfer(&repo, cancel, OP)?;

    let updated_refs = match scheme {
        RemoteScheme::Https => {
            let creds = (auth.creds.cloned(), auth.host.unwrap_or(&host).to_owned());
            let worker_cancel = cancel.clone();
            let (remote_name, host) = (remote_name.clone(), host.clone());
            run_detached(cancel, OP, move || {
                // Held by the worker, so an abandoned fetch keeps the repository claimed
                // until it really ends.
                let _claim = claim;
                fetch_https(
                    &repo,
                    &proof,
                    creds,
                    (&remote_name, &url, &host),
                    &worker_cancel,
                )
            })?
        }
        RemoteScheme::Ssh => {
            let transport = super::ssh::transport(
                &url,
                auth,
                super::ssh::Exec {
                    service: gix::protocol::transport::Service::UploadPack,
                    version: gix::protocol::transport::Protocol::V2,
                },
                cancel,
                OP,
            )?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
            )]
            let connection = remote
                .to_connection_with_transport(transport)
                .with_credentials(authenticate_with(None, String::new()));
            let writer = Writer::new(&repo, &proof);
            updated_refs(&run_fetch(
                &writer,
                connection,
                (&remote_name, &host),
                cancel,
            )?)
        }
    };

    Ok(FetchOutcome {
        remote: remote_name,
        updated_refs,
    })
}

/// Where a fetch reads from: the checked-out branch's upstream, or on a detached `HEAD`
/// `origin`, since a fetch needs no branch.
fn fetch_source(repo: &gix::Repository) -> ActionResult<Upstream> {
    let head_name = repo.head_name().map_err(git_err(OP, "HEAD"))?;
    let (branch, upstream) = match head_name {
        Some(head_name) => {
            let branch = head_name.shorten().to_string();
            let upstream = resolve_upstream(repo, &branch, gix::remote::Direction::Fetch, OP)?;
            (branch, upstream)
        }
        None => ("HEAD".to_owned(), detached_upstream(repo, OP)?),
    };
    upstream.ok_or(ActionError::NoUpstream { op: OP, branch })
}

/// The named remote of `repo`.
fn find_remote<'repo>(
    repo: &'repo gix::Repository,
    remote_name: &str,
) -> ActionResult<gix::Remote<'repo>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    repo.find_remote(remote_name)
        .map_err(git_err(OP, remote_name))
}

/// The whole HTTPS fetch: connect, negotiate, receive. It runs on the detached thread
/// [`run_detached`] starts, because curl blocks in reads the cancel flag cannot reach;
/// `cancel` still stops it at the next object once the caller has given up waiting.
fn fetch_https(
    repo: &gix::Repository,
    proof: &Confirmation,
    (creds, creds_host): (Option<Credentials>, String),
    (remote_name, url, host): (&str, &gix::Url, &str),
    cancel: &Cancel,
) -> ActionResult<Vec<RefUpdate>> {
    let remote = find_remote(repo, remote_name)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|source| map_network_error(Box::new(source), OP, remote_name, host))?;
    let mut connection = connection.with_credentials(authenticate_with(creds, creds_host));
    connection.set_transport_options(Box::new(http_options(url, cancel)));
    let writer = Writer::new(repo, proof);
    Ok(updated_refs(&run_fetch(
        &writer,
        connection,
        (remote_name, host),
        cancel,
    )?))
}

/// Runs the prepared connection through negotiation and pack receive, mapping errors
/// the same way for both transports. The receive writes the pack and the tracking refs, so it
/// takes the action's `_writer` as proof the policy allowed it; `clippy.toml` bans `receive`
/// everywhere else.
fn run_fetch<T>(
    _writer: &Writer<'_>,
    connection: gix::remote::Connection<'_, '_, '_, T>,
    (remote_name, host): (&str, &str),
    cancel: &Cancel,
) -> ActionResult<gix::remote::fetch::Outcome>
where
    T: gix::protocol::transport::client::blocking_io::Transport,
{
    let prepared = connection
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|source| {
            if cancel.is_cancelled() {
                return ActionError::Cancelled { op: OP };
            }
            map_network_error(Box::new(source), OP, remote_name, host)
        })?;
    // gix moves the tracking refs inside `receive` with no seam before them, and checks the
    // flag only while writing the pack, so this is the last point to stop before any write.
    bail_if_cancelled(cancel, OP)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "receive writes the pack and tracking refs; `_writer` proves the policy allowed it"
    )]
    prepared
        .receive(gix::progress::Discard, cancel.flag())
        .map_err(|source| {
            if cancel.is_cancelled() {
                return ActionError::Cancelled { op: OP };
            }
            map_network_error(Box::new(source), OP, remote_name, host)
        })
}

/// The refs a fetch actually created or moved; refs the edits merely asserted
/// (`NoChangeNeeded`) are skipped.
fn updated_refs(outcome: &gix::remote::fetch::Outcome) -> Vec<RefUpdate> {
    use gix::remote::fetch::Status;
    use gix::remote::fetch::refs::update::Mode;
    let update_refs = match &outcome.status {
        Status::NoPackReceived { update_refs, .. } | Status::Change { update_refs, .. } => {
            update_refs
        }
    };
    let mut updates = Vec::new();
    for update in &update_refs.updates {
        let changed = matches!(update.mode, Mode::New | Mode::FastForward | Mode::Forced);
        let Some(edit) = update
            .edit_index
            .and_then(|index| update_refs.edits.get(index))
        else {
            continue;
        };
        if !changed {
            continue;
        }
        if let gix::refs::transaction::Change::Update {
            new: gix::refs::Target::Object(id),
            ..
        } = &edit.change
        {
            updates.push(RefUpdate {
                name: edit.name.as_bstr().to_string(),
                target: *id,
            });
        }
    }
    updates
}
