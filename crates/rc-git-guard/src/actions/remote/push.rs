//! Plain (never force) push of the checked-out branch to its upstream over HTTPS.
//! gix has no push, so this drives the smart protocol by hand: handshake for the
//! advertisement, a client-side fast-forward check before anything is sent, one
//! command pkt-line plus a self-contained pack, then the server's `report-status`.

use std::io::{Read, Write};
use std::path::Path;

use gix::ObjectId;
use gix::refs::Target;
use gix::refs::transaction::PreviousValue;
use gix::remote::Direction;

use crate::actions::treeapply::core_oid;
use crate::actions::write::Writer;
use crate::actions::{ActionError, ActionResult, authorize, open};
use crate::{Action, Confirmation, Policy};

use super::report::Report;
use super::{
    Cancel, Credentials, RemoteAuth, RemoteScheme, Upstream, authenticate_with, bail_if_cancelled,
    current_branch, host_of, http_options, is_ancestor, map_network_error, pack, push_refs, report,
    resolve_upstream, run_detached, set_reflog_identity, vet_remote_url,
};

const OP: &str = "push";

type BoxedTransport = Box<dyn gix::protocol::transport::client::blocking_io::Transport + Send>;

/// What a push did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    /// The remote pushed to.
    pub remote: String,
    /// Short name of the branch pushed.
    pub branch: String,
    /// The remote's tip before the push; `None` when the push created the branch.
    pub old: Option<rc_git_core::Oid>,
    /// The remote's tip now: the local tip. Equal to `old` when there was nothing to push.
    pub new: rc_git_core::Oid,
    /// Commits uploaded.
    pub pushed_commits: usize,
    /// Whether the remote already had the local tip, so nothing was sent.
    pub already_up_to_date: bool,
    /// Why the remote-tracking ref still points at the old tip, when moving it failed. The
    /// push itself landed; ahead/behind reads stale until a fetch or another push moves it.
    pub tracking_ref_error: Option<String>,
}

/// Pushes the checked-out branch to the same-named branch on its push remote:
/// `branch.<name>.pushRemote`, then `remote.pushDefault`, then the upstream remote, at its
/// `pushurl` when it has one. Plain push only: the remote tip must be an ancestor of the
/// local tip, checked here before a byte is sent and enforced again by the server. The
/// remote-tracking ref moved is the pushed remote's; a URL upstream has none to move.
///
/// # Errors
/// [`ActionError::PushRejected`] when the remote has commits the local branch lacks,
/// [`ActionError::AuthRequired`], [`ActionError::UnsupportedRemote`],
/// [`ActionError::NoUpstream`], [`ActionError::Cancelled`], [`ActionError::DetachedHead`],
/// and [`ActionError::Git`] for every other failure, including a local upstream
/// (`branch.<name>.remote = .`), which rc-git does not push into, and a
/// `branch.<name>.merge` that names another branch, which rc-git does not push to.
pub fn push_upstream(
    repo_path: &Path,
    policy: &Policy,
    auth: RemoteAuth<'_>,
    cancel: &Cancel,
) -> ActionResult<PushOutcome> {
    let mut repo = open(repo_path, OP, "repository")?;
    set_reflog_identity(&mut repo, OP)?;
    let repo = repo;
    let (branch, tip) = current_branch(&repo, OP)?;
    let action = Action::Push {
        branch: branch.clone(),
        force: false,
    };
    let proof = authorize(policy, &action, OP, &branch, None)?;
    bail_if_cancelled(cancel, OP)?;
    let tip = tip.ok_or_else(|| {
        ActionError::git(
            OP,
            branch.clone(),
            "the branch has no commits yet; nothing to push",
        )
    })?;
    let (upstream, url) = push_target(&repo, &branch)?;
    let (full_ref, tracking_ref) = push_refs(&repo, &branch, &upstream, OP)?;
    let remote_name = upstream.label();
    let scheme = vet_remote_url(&url, OP, &remote_name)?;
    let host = host_of(&url);

    let transport = match scheme {
        RemoteScheme::Https => {
            let (url, host, worker_cancel) = (url.clone(), host.clone(), cancel.clone());
            run_detached(cancel, OP, move || connect(&url, &host, &worker_cancel))?
        }
        RemoteScheme::Ssh => super::ssh::transport(
            &url,
            auth,
            gix::protocol::transport::Service::ReceivePack,
            gix::protocol::transport::Protocol::V1,
            cancel,
            OP,
        )?,
    };
    let link = Link {
        host,
        full_ref,
        cancel: cancel.clone(),
    };
    let (transport, refs) = {
        let (link, creds) = (link.clone(), auth.creds.cloned());
        run_detached(cancel, OP, move || advertise(transport, &link, creds))?
    };
    let (remote_old, haves) = remote_tips(&repo, &refs, &link.full_ref);
    let old_id = remote_old.map(|id| core_oid(id, OP)).transpose()?;
    let new_id = core_oid(tip, OP)?;
    if remote_old == Some(tip) {
        return Ok(PushOutcome {
            remote: remote_name,
            branch,
            old: old_id,
            new: new_id,
            pushed_commits: 0,
            already_up_to_date: true,
            tracking_ref_error: move_tracking_ref(&repo, &proof, tracking_ref.as_deref(), tip),
        });
    }
    if let Some(old) = remote_old
        && (repo.find_header(old).is_err() || !is_ancestor(&repo, old, tip))
    {
        return Err(ActionError::PushRejected { branch });
    }
    bail_if_cancelled(cancel, OP)?;

    let pack = pack::build_pack(&repo, tip, &haves, cancel)?;
    bail_if_cancelled(cancel, OP)?;
    let pushed_commits = pack.commits;
    {
        let (link, bytes) = (link.clone(), pack.bytes);
        run_detached(cancel, OP, move || {
            send(transport, &link, remote_old, tip, &bytes)
        })?;
    }
    Ok(PushOutcome {
        remote: remote_name,
        branch,
        old: old_id,
        new: new_id,
        pushed_commits,
        already_up_to_date: false,
        tracking_ref_error: move_tracking_ref(&repo, &proof, tracking_ref.as_deref(), tip),
    })
}

/// Where the branch pushes, resolved in git's push order, with the URL it pushes to. A local
/// upstream is refused: rc-git does not push into a local branch.
fn push_target(repo: &gix::Repository, branch: &str) -> ActionResult<(Upstream, gix::Url)> {
    let upstream = resolve_upstream(repo, branch, Direction::Push, OP)?.ok_or_else(|| {
        ActionError::NoUpstream {
            op: OP,
            branch: branch.to_owned(),
        }
    })?;
    match &upstream {
        Upstream::Local { merge_ref } => Err(ActionError::git(
            OP,
            branch,
            format!(
                "the upstream is the local branch {merge_ref}; rc-git does not push into a \
                 local branch (merge into it with git, or set a remote upstream)"
            ),
        )),
        Upstream::Named { url, .. } | Upstream::Url { url } => {
            let url = url.clone();
            Ok((upstream, url))
        }
    }
}

/// The remote end of one push: what its network errors are reported against, and the
/// flag that turns any of them into [`ActionError::Cancelled`]. Owned, so each network
/// step can carry a copy onto the detached thread [`run_detached`] runs it on.
#[derive(Clone)]
struct Link {
    host: String,
    full_ref: String,
    cancel: Cancel,
}

impl Link {
    /// Maps a transfer failure. A cancel ends the transport's reads and writes with an
    /// I/O error, so the flag is looked at first and wins over whatever that error says.
    fn failed(&self, source: Box<dyn std::error::Error + Send + Sync + 'static>) -> ActionError {
        if self.cancel.is_cancelled() {
            return ActionError::Cancelled { op: OP };
        }
        map_network_error(source, OP, &self.full_ref, &self.host)
    }

    fn bail_if_cancelled(&self) -> ActionResult<()> {
        bail_if_cancelled(&self.cancel, OP)
    }
}

/// Bytes handed to the transport per write, so a cancel lands between chunks of a large
/// pack instead of after the whole upload.
const PACK_CHUNK: usize = 64 * 1024;

/// Handshakes with `git-receive-pack` on `transport` (the HTTP `GET /info/refs`) and
/// returns the transport with the refs the server advertised.
fn advertise(
    mut transport: BoxedTransport,
    link: &Link,
    creds: Option<Credentials>,
) -> ActionResult<(BoxedTransport, Vec<gix::protocol::handshake::Ref>)> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let handshake = gix::protocol::handshake(
        &mut transport,
        gix::protocol::transport::Service::ReceivePack,
        authenticate_with(creds),
        Vec::new(),
        &mut gix::progress::Discard,
    )
    .map_err(|source| link.failed(Box::new(source)))?;
    Ok((transport, handshake.refs.unwrap_or_default()))
}

/// The advertised tip of `full_ref` plus every advertised tip present locally (the
/// remote's "haves").
fn remote_tips(
    repo: &gix::Repository,
    refs: &[gix::protocol::handshake::Ref],
    full_ref: &str,
) -> (Option<ObjectId>, Vec<ObjectId>) {
    let mut remote_old = None;
    let mut haves = Vec::new();
    for advertised in refs {
        let (name, target) = match advertised {
            gix::protocol::handshake::Ref::Direct {
                full_ref_name,
                object,
            }
            | gix::protocol::handshake::Ref::Peeled {
                full_ref_name,
                object,
                ..
            }
            | gix::protocol::handshake::Ref::Symbolic {
                full_ref_name,
                object,
                ..
            } => (full_ref_name, *object),
            gix::protocol::handshake::Ref::Unborn { .. } => continue,
        };
        if name == full_ref {
            remote_old = Some(target);
        }
        if repo.find_header(target).is_ok() {
            haves.push(target);
        }
    }
    (remote_old, haves)
}

/// Sends the command line and the pack over one `git-receive-pack` request (the HTTP
/// `POST`, on the transport the handshake prepared) and reads the server's
/// `report-status`. The pack goes out in [`PACK_CHUNK`] pieces and the report comes in
/// one read at a time, with the cancel flag looked at before each.
fn send(
    mut transport: BoxedTransport,
    link: &Link,
    remote_old: Option<ObjectId>,
    tip: ObjectId,
    pack: &[u8],
) -> ActionResult<()> {
    let full_ref = link.full_ref.as_str();
    let net = |source: std::io::Error| link.failed(Box::new(source));
    let mut writer = transport
        .request(
            gix::protocol::transport::client::WriteMode::OneLfTerminatedLinePerWriteCall,
            gix::protocol::transport::client::MessageKind::Flush,
            false,
        )
        .map_err(|source| link.failed(Box::new(source)))?;
    writer
        .write_all(&report::command_line(remote_old, tip, full_ref))
        .map_err(net)?;
    writer
        .write_message(gix::protocol::transport::client::MessageKind::Flush)
        .map_err(net)?;
    let (mut raw, mut reader) = writer.into_parts();
    for chunk in pack.chunks(PACK_CHUNK) {
        link.bail_if_cancelled()?;
        raw.write_all(chunk).map_err(net)?;
    }
    raw.flush().map_err(net)?;
    drop(raw);

    let mut payload = Vec::new();
    let mut buf = [0_u8; 8 * 1024];
    loop {
        link.bail_if_cancelled()?;
        let read = reader.read(&mut buf).map_err(net)?;
        if read == 0 {
            break;
        }
        payload.extend_from_slice(buf.get(..read).unwrap_or_default());
    }
    match report::parse_report(&payload, full_ref) {
        Ok(Report::Ok) => Ok(()),
        Ok(Report::Refused {
            reason,
            non_fast_forward,
        }) => {
            if non_fast_forward {
                return Err(ActionError::PushRejected {
                    branch: full_ref
                        .strip_prefix("refs/heads/")
                        .unwrap_or(full_ref)
                        .to_owned(),
                });
            }
            Err(ActionError::git(
                OP,
                full_ref,
                format!("the server refused the push: {reason}"),
            ))
        }
        Err(message) => Err(ActionError::git(OP, full_ref, message)),
    }
}

/// Opens the HTTP transport for `url`, with rc-git's connect and idle timeouts on it.
/// `connect::Options` carries no HTTP settings, so they go on afterwards through the
/// transport's own `configure`; without them curl waits forever on a dead peer.
fn connect(url: &gix::Url, host: &str, cancel: &Cancel) -> ActionResult<BoxedTransport> {
    use gix::protocol::transport::client::TransportWithoutIO;
    let failed = |source: Box<dyn std::error::Error + Send + Sync + 'static>| {
        connect_failed(url, host, source)
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let mut transport = gix::protocol::transport::client::blocking_io::connect::connect(
        url.clone(),
        gix::protocol::transport::client::blocking_io::connect::Options {
            version: gix::protocol::transport::Protocol::V1,
            ..Default::default()
        },
    )
    .map_err(|source| failed(Box::new(source)))?;
    transport
        .configure(&http_options(url, cancel))
        .map_err(failed)?;
    Ok(transport)
}

/// Maps a connect or configure failure, naming the URL without its userinfo so a token
/// in the remote URL never reaches the message.
fn connect_failed(
    url: &gix::Url,
    host: &str,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
) -> ActionError {
    map_network_error(source, OP, super::redacted(url).as_str(), host)
}

/// Moves the remote-tracking ref to the tip the remote now has, unless it is already there,
/// and returns why that failed. Runs only once the remote has the tip, so a failure here is
/// reported beside the push rather than as its failure; a later push repairs the ref.
fn move_tracking_ref(
    repo: &gix::Repository,
    proof: &Confirmation,
    tracking_ref: Option<&str>,
    tip: ObjectId,
) -> Option<String> {
    let tracking_ref = tracking_ref?;
    // An unreadable ref only skips the shortcut: the write below reads it again and reports.
    let current = repo
        .try_find_reference(tracking_ref)
        .ok()
        .flatten()
        .and_then(|reference| reference.target().try_id().map(ToOwned::to_owned));
    if current == Some(tip) {
        return None;
    }
    update_tracking_ref(&Writer::new(repo, proof), tracking_ref, tip)
        .err()
        .map(|err| err.to_string())
}

/// Moves the remote-tracking ref status counts against to the pushed tip so ahead/behind
/// reads 0 without another fetch.
fn update_tracking_ref(writer: &Writer<'_>, full_name: &str, tip: ObjectId) -> ActionResult<()> {
    let edit = crate::actions::update_edit(
        full_name,
        PreviousValue::Any,
        Target::Object(tip),
        "rc-git: push",
        OP,
    )?;
    writer.apply_edit(edit, OP, full_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connect_failure_names_the_url_without_its_userinfo() {
        let url = gix::Url::from_bytes("https://user:secret@example.com/x.git".into())
            .expect("parse url");
        let err = connect_failed(&url, "example.com", "connection refused".into());
        let message = err.to_string();
        assert!(!message.contains("secret"), "{message}");
        assert!(!message.contains("user@"), "{message}");
        assert!(message.contains("https://example.com/x.git"), "{message}");
    }
}
