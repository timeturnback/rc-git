//! Plain (never force) push of the checked-out branch to its upstream over HTTPS.
//! gix has no push, so this drives the smart protocol by hand: handshake for the
//! advertisement, a client-side fast-forward check before anything is sent, one
//! command pkt-line plus a self-contained pack, then the server's `report-status`.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::ObjectId;

use crate::actions::{ActionError, ActionResult, confirm, open};
use crate::{Action, Policy};

use super::report::Report;
use super::{
    Credentials, RemoteAuth, RemoteScheme, authenticate_with, bail_if_cancelled,
    configured_remote_name, current_branch, host_of, is_ancestor, map_network_error, pack, report,
    set_reflog_identity, vet_remote_url,
};

const OP: &str = "push";

/// What a push did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOutcome {
    /// The remote pushed to.
    pub remote: String,
    /// Short name of the branch pushed.
    pub branch: String,
    /// The remote's tip before the push; `None` when the push created the branch.
    pub old: Option<ObjectId>,
    /// The remote's tip now: the local tip. Equal to `old` when there was nothing to push.
    pub new: ObjectId,
    /// Commits uploaded.
    pub pushed_commits: usize,
}

/// Pushes the checked-out branch to its upstream remote. Plain push only: the remote
/// tip must be an ancestor of the local tip, checked here before a byte is sent and
/// enforced again by the server.
///
/// # Errors
/// [`ActionError::PushRejected`] when the remote has commits the local branch lacks,
/// [`ActionError::AuthRequired`], [`ActionError::UnsupportedRemote`],
/// [`ActionError::NoUpstream`], [`ActionError::Cancelled`], [`ActionError::DetachedHead`],
/// and [`ActionError::Git`] for every other failure.
pub fn push_upstream(
    repo_path: &Path,
    policy: &Policy,
    auth: RemoteAuth<'_>,
    cancel: &AtomicBool,
) -> ActionResult<PushOutcome> {
    let mut repo = open(repo_path, OP, "repository")?;
    set_reflog_identity(&mut repo, OP)?;
    let repo = repo;
    let (branch, tip) = current_branch(&repo, OP)?;
    let decision = policy.evaluate(&Action::Push {
        branch: branch.clone(),
        force: false,
    });
    let _proof = confirm(decision, OP, &branch, None)?;
    bail_if_cancelled(cancel, OP)?;
    let tip = tip.ok_or_else(|| ActionError::Git {
        op: OP,
        name: branch.clone(),
        source: "the branch has no commits yet; nothing to push".into(),
    })?;
    let remote_name = configured_remote_name(&repo, &branch).ok_or(ActionError::NoUpstream {
        op: OP,
        branch: branch.clone(),
    })?;
    let url = super::remote_fetch_url(&repo, &remote_name, OP)?.ok_or_else(|| {
        ActionError::NoUpstream {
            op: OP,
            branch: branch.clone(),
        }
    })?;
    let scheme = vet_remote_url(&url, OP, &remote_name)?;
    let host = host_of(&url);
    let full_ref = format!("refs/heads/{branch}");

    let mut transport = match scheme {
        RemoteScheme::Https => connect(&url, &host)?,
        RemoteScheme::Ssh => super::ssh::transport(
            &url,
            auth,
            gix::protocol::transport::Service::ReceivePack,
            gix::protocol::transport::Protocol::V1,
            cancel,
            OP,
        )?,
    };
    let (remote_old, haves) = advertise(&repo, &mut transport, &host, auth.creds, &full_ref)?;
    if remote_old == Some(tip) {
        return Ok(PushOutcome {
            remote: remote_name,
            branch,
            old: remote_old,
            new: tip,
            pushed_commits: 0,
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
    send(
        &mut transport,
        &host,
        remote_old,
        tip,
        &full_ref,
        &pack.bytes,
    )?;
    update_tracking_ref(&repo, &remote_name, &branch, tip)?;
    Ok(PushOutcome {
        remote: remote_name,
        branch,
        old: remote_old,
        new: tip,
        pushed_commits: pack.commits,
    })
}

/// Handshakes with `git-receive-pack` on `transport` (the HTTP `GET /info/refs`) and
/// returns the advertised tip of `full_ref` plus every advertised tip present locally
/// (the remote's "haves").
fn advertise(
    repo: &gix::Repository,
    transport: &mut Box<dyn gix::protocol::transport::client::blocking_io::Transport + Send>,
    host: &str,
    creds: Option<&Credentials>,
    full_ref: &str,
) -> ActionResult<(Option<ObjectId>, Vec<ObjectId>)> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    let handshake = gix::protocol::handshake(
        transport,
        gix::protocol::transport::Service::ReceivePack,
        authenticate_with(creds.cloned()),
        Vec::new(),
        &mut gix::progress::Discard,
    )
    .map_err(|source| map_network_error(Box::new(source), OP, full_ref, host))?;
    let refs = handshake.refs.unwrap_or_default();
    let mut remote_old = None;
    let mut haves = Vec::new();
    for advertised in &refs {
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
    Ok((remote_old, haves))
}

/// Sends the command line and the pack over one `git-receive-pack` request (the HTTP
/// `POST`, on the transport the handshake prepared) and reads the server's
/// `report-status`.
fn send(
    transport: &mut Box<dyn gix::protocol::transport::client::blocking_io::Transport + Send>,
    host: &str,
    remote_old: Option<ObjectId>,
    tip: ObjectId,
    full_ref: &str,
    pack: &[u8],
) -> ActionResult<()> {
    let net = |source: std::io::Error| map_network_error(Box::new(source), OP, full_ref, host);
    let mut writer = transport
        .request(
            gix::protocol::transport::client::WriteMode::OneLfTerminatedLinePerWriteCall,
            gix::protocol::transport::client::MessageKind::Flush,
            false,
        )
        .map_err(|source| map_network_error(Box::new(source), OP, full_ref, host))?;
    writer
        .write_all(&report::command_line(remote_old, tip, full_ref))
        .map_err(net)?;
    writer
        .write_message(gix::protocol::transport::client::MessageKind::Flush)
        .map_err(net)?;
    let (mut raw, mut reader) = writer.into_parts();
    raw.write_all(pack).map_err(net)?;
    raw.flush().map_err(net)?;
    drop(raw);

    let mut payload = Vec::new();
    reader.read_to_end(&mut payload).map_err(net)?;
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
            Err(ActionError::Git {
                op: OP,
                name: full_ref.to_owned(),
                source: format!("the server refused the push: {reason}").into(),
            })
        }
        Err(message) => Err(ActionError::Git {
            op: OP,
            name: full_ref.to_owned(),
            source: message.into(),
        }),
    }
}

/// Opens the HTTP transport for `url`.
fn connect(
    url: &gix::Url,
    host: &str,
) -> ActionResult<Box<dyn gix::protocol::transport::client::blocking_io::Transport + Send>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
    )]
    gix::protocol::transport::client::blocking_io::connect::connect(
        url.clone(),
        gix::protocol::transport::client::blocking_io::connect::Options {
            version: gix::protocol::transport::Protocol::V1,
            ..Default::default()
        },
    )
    .map_err(|source| {
        map_network_error(
            Box::new(source),
            OP,
            url.to_bstring().to_string().as_str(),
            host,
        )
    })
}

/// Moves `refs/remotes/<remote>/<branch>` to the pushed tip so ahead/behind reads 0
/// without another fetch.
fn update_tracking_ref(
    repo: &gix::Repository,
    remote: &str,
    branch: &str,
    tip: ObjectId,
) -> ActionResult<()> {
    use gix::refs::Target;
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    let full_name = format!("refs/remotes/{remote}/{branch}");
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "rc-git: push".into(),
            },
            expected: PreviousValue::Any,
            new: Target::Object(tip),
        },
        name: full_name.as_str().try_into().map_err(
            |source: gix::validate::reference::name::Error| ActionError::Git {
                op: OP,
                name: full_name.clone(),
                source: Box::new(source),
            },
        )?,
        deref: false,
    };
    crate::actions::apply_edit(repo, edit, OP, &full_name)
}
