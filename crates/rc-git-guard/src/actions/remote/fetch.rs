//! Fetch from the checked-out branch's remote over HTTPS, updating remote-tracking
//! refs exactly as `git fetch <remote>` would. gix writes the received pack and the
//! tracking refs itself; nothing else is touched, and `FETCH_HEAD` is never written.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::actions::{ActionError, ActionResult, confirm, open};
use crate::{Action, Policy};

use super::{
    Credentials, authenticate_with, bail_if_cancelled, configured_remote_name, current_branch,
    host_of, map_network_error, require_https,
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
/// using the remote's own refspecs. HTTPS only; `creds` answer the server's 401.
///
/// # Errors
/// [`ActionError::UnsupportedRemote`] for a non-HTTPS remote, [`ActionError::AuthRequired`]
/// when the server rejects the credentials, [`ActionError::Cancelled`] when `cancel` is
/// set, [`ActionError::NoUpstream`] when no remote is configured, [`ActionError::Git`]
/// for every other failure.
pub fn fetch_upstream(
    repo_path: &Path,
    policy: &Policy,
    creds: Option<&Credentials>,
    cancel: &AtomicBool,
) -> ActionResult<FetchOutcome> {
    let decision = policy.evaluate(&Action::Fetch);
    let _proof = confirm(decision, OP, "remote", None)?;
    bail_if_cancelled(cancel, OP)?;
    let mut repo = open(repo_path, OP, "repository")?;
    // The tracking-ref updates write reflog lines, which need a committer the
    // isolated open does not provide. In-memory only; the repository's own
    // configuration file is never touched.
    {
        let mut config = repo.config_snapshot_mut();
        let set = |config: &mut gix::config::SnapshotMut<'_>,
                   key: &'static dyn gix::config::tree::Key,
                   value: &str|
         -> ActionResult<()> {
            config
                .set_value(key, value)
                .map_err(|source| ActionError::Git {
                    op: OP,
                    name: "configuration".to_owned(),
                    source: Box::new(source),
                })?;
            Ok(())
        };
        set(&mut config, &gix::config::tree::User::NAME, "rc-git")?;
        set(
            &mut config,
            &gix::config::tree::User::EMAIL,
            "rc-git@localhost",
        )?;
        config.commit().map_err(|source| ActionError::Git {
            op: OP,
            name: "configuration".to_owned(),
            source: Box::new(source),
        })?;
    }
    let repo = repo;
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
        .map_err(|source| ActionError::Git {
            op: OP,
            name: remote_name.clone(),
            source: Box::new(source),
        })?;
    let url = remote
        .url(gix::remote::Direction::Fetch)
        .ok_or_else(|| ActionError::NoUpstream {
            op: OP,
            branch: branch.clone(),
        })?
        .clone();
    require_https(&url, OP, &remote_name)?;
    let host = host_of(&url);

    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let connection = remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|source| map_network_error(Box::new(source), OP, &remote_name, &host))?;
    let outcome = connection
        .with_credentials(authenticate_with(creds.cloned()))
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(|source| map_network_error(Box::new(source), OP, &remote_name, &host))?
        .receive(gix::progress::Discard, cancel)
        .map_err(|source| {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                return ActionError::Cancelled { op: OP };
            }
            map_network_error(Box::new(source), OP, &remote_name, &host)
        })?;

    Ok(FetchOutcome {
        remote: remote_name,
        updated_refs: updated_refs(&outcome),
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
