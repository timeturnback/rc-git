//! Fetch from the checked-out branch's remote over HTTPS, updating remote-tracking
//! refs exactly as `git fetch <remote>` would. gix writes the received pack and the
//! tracking refs itself; nothing else is touched, and `FETCH_HEAD` is never written.

use std::path::Path;

use crate::actions::write::Writer;
use crate::actions::{ActionError, ActionResult, authorize, git_err, open};
use crate::{Action, Policy};

use super::{
    Cancel, RemoteAuth, RemoteScheme, authenticate_with, bail_if_cancelled, configured_remote_name,
    current_branch, host_of, http_options, map_network_error, vet_remote_url,
};

const OP: &str = "fetch";

/// One remote-tracking ref a fetch created or moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    /// Full ref name, e.g. `refs/remotes/origin/main`.
    pub name: String,
    /// The commit it now points at.
    pub target: gix::ObjectId,
}

/// What a fetch changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// The remote fetched from.
    pub remote: String,
    /// Remote-tracking refs created or moved, empty when everything was up to date.
    pub updated_refs: Vec<RefUpdate>,
}

/// Fetches from the checked-out branch's remote (`origin` when none is configured),
/// using the remote's own refspecs. `auth.creds` answer an HTTPS server's 401.
///
/// # Errors
/// [`ActionError::UnsupportedRemote`] for a remote scheme rc-git does not speak,
/// [`ActionError::AuthRequired`] when the server rejects the credentials,
/// [`ActionError::Cancelled`] when `cancel` is set, [`ActionError::NoUpstream`] when no
/// remote is configured, [`ActionError::Git`] for every other failure.
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
    let writer = Writer::new(&repo, &proof);
    let (branch, _tip) = current_branch(&repo, OP)?;
    let remote_name = configured_remote_name(&repo, &branch).ok_or(ActionError::NoUpstream {
        op: OP,
        branch: branch.clone(),
    })?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let remote = repo
        .find_remote(remote_name.as_str())
        .map_err(git_err(OP, &remote_name))?;
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .ok_or_else(|| ActionError::NoUpstream {
            op: OP,
            branch: branch.clone(),
        })?
        .clone();
    let scheme = vet_remote_url(&url, OP, &remote_name)?;
    let host = host_of(&url);

    let outcome = match scheme {
        RemoteScheme::Https => {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
            )]
            let connection = remote
                .connect(gix::remote::Direction::Fetch)
                .map_err(|source| map_network_error(Box::new(source), OP, &remote_name, &host))?;
            let mut connection =
                connection.with_credentials(authenticate_with(auth.creds.cloned()));
            connection.set_transport_options(Box::new(http_options(&url, cancel)));
            run_fetch(&writer, connection, (&remote_name, &host), cancel)?
        }
        RemoteScheme::Ssh => {
            let transport = super::ssh::transport(
                &url,
                auth,
                gix::protocol::transport::Service::UploadPack,
                gix::protocol::transport::Protocol::V2,
                cancel,
                OP,
            )?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
            )]
            let connection = remote
                .to_connection_with_transport(transport)
                .with_credentials(authenticate_with(None));
            run_fetch(&writer, connection, (&remote_name, &host), cancel)?
        }
    };

    Ok(FetchOutcome {
        remote: remote_name,
        updated_refs: updated_refs(&outcome),
    })
}

/// Runs the prepared connection through negotiation and pack receive, mapping errors
/// the same way for both transports. The receive writes the pack and the tracking refs, so it
/// takes the action's `_writer` as proof the policy allowed it.
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
