//! The ssh transport: an in-process SSH client (russh) running `git-upload-pack` /
//! `git-receive-pack` on the server and speaking the git wire protocol over the
//! channel. No child process is ever spawned — the same invariant the HTTPS path
//! keeps. `known_hosts` is consulted read-only; trust decisions live with the caller.

use std::sync::{Arc, Mutex, PoisonError};

use gix::protocol::transport::client::git::ConnectMode;
use gix::protocol::transport::{Protocol, Service};
use russh::client::{Handle, Msg};

use crate::actions::{ActionError, ActionResult};

use super::{CONNECT_TIMEOUT, Cancel, RemoteAuth, bail_if_cancelled};

mod auth;
mod hostkey;
mod session;
mod target;

use hostkey::{HostKeyHandler, HostKeyReport, ReportSlot};
use session::{SessionHold, SshRead, SshWrite};
use target::SshTarget;

/// Boxed transport type shared with the HTTPS push path.
type BoxedTransport = Box<dyn gix::protocol::transport::client::blocking_io::Transport + Send>;

/// Opens an ssh connection to `url`'s host, authenticates, execs the git service, and
/// wraps the channel as a blocking git-protocol transport. The transport keeps a share
/// of `cancel`, so setting the flag after this returns still stops the transfer.
///
/// # Errors
/// [`ActionError::UnknownHostKey`] and [`ActionError::HostKeyMismatch`] from host-key
/// verification, [`ActionError::PassphraseRequired`] and [`ActionError::AuthRequired`]
/// from authentication, [`ActionError::Cancelled`] when `cancel` is set before the
/// dial, [`ActionError::Git`] for every other failure.
pub(super) fn transport(
    url: &gix::Url,
    auth: RemoteAuth<'_>,
    service: Service,
    desired_version: Protocol,
    cancel: &Cancel,
    op: &'static str,
) -> ActionResult<BoxedTransport> {
    let target = target::resolve(url, auth.ssh_dir, op)?;
    let command = target::quoted_command(service, url.path.as_ref(), op)?;
    bail_if_cancelled(cancel, op)?;
    let runtime = session::build_runtime(op)?;
    let report: ReportSlot = Arc::new(Mutex::new(None));
    let handler = HostKeyHandler::new(&target, auth.accepted_hostkey, Arc::clone(&report));
    let passphrase = auth.creds.map(|creds| creds.token.as_str());
    let connected = runtime.block_on(connect_and_exec(
        &target,
        handler,
        passphrase,
        &command,
        service,
        desired_version,
        op,
    ));
    let (client, channel) = match connected {
        Ok(pair) => pair,
        Err(err) => return Err(typed_refusal(&report, &target, op).unwrap_or(err)),
    };
    bail_if_cancelled(cancel, op)?;
    let (read_half, write_half) = tokio::io::split(channel.into_stream());
    let handle = runtime.handle().clone();
    let hold = SessionHold::new(client, runtime);
    let watch = session::Watch::new(cancel, op);
    let connection = gix::protocol::transport::client::git::blocking_io::Connection::new(
        SshRead::new(read_half, handle.clone(), &hold, watch.clone()),
        SshWrite::new(write_half, handle, &hold, watch),
        desired_version,
        url.path.clone(),
        None::<(String, _)>,
        ConnectMode::Process,
        false,
    )
    .custom_url(Some(super::redacted(url).into()));
    Ok(Box::new(connection))
}

/// Dial, verify the host key, authenticate, open a session channel, and exec the git
/// service on it. Runs on the transport's private runtime.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
)]
async fn connect_and_exec(
    target: &SshTarget,
    handler: HostKeyHandler,
    passphrase: Option<&str>,
    command: &str,
    service: Service,
    desired_version: Protocol,
    op: &'static str,
) -> ActionResult<(Handle<HostKeyHandler>, russh::Channel<Msg>)> {
    let config = Arc::new(russh::client::Config::default());
    let address = (target.host_name.as_str(), target.port);
    let mut client = tokio::time::timeout(
        CONNECT_TIMEOUT,
        russh::client::connect(config, address, handler),
    )
    .await
    .map_err(|_| timeout_err(op, target))?
    .map_err(|source| connection_err(op, target, source))?;
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        auth::authenticate(&mut client, target, passphrase, op),
    )
    .await
    .map_err(|_| timeout_err(op, target))??;
    let channel = client
        .channel_open_session()
        .await
        .map_err(|source| connection_err(op, target, source))?;
    if service == Service::UploadPack && desired_version == Protocol::V2 {
        // Best effort: a server that ignores the env just advertises V0/V1, which the
        // handshake accepts as a downgrade.
        channel
            .set_env(false, "GIT_PROTOCOL", "version=2")
            .await
            .map_err(|source| connection_err(op, target, source))?;
    }
    channel
        .exec(false, command)
        .await
        .map_err(|source| connection_err(op, target, source))?;
    Ok((client, channel))
}

/// The typed error for a host-key refusal recorded by the handler, when there is one.
fn typed_refusal(report: &ReportSlot, target: &SshTarget, op: &'static str) -> Option<ActionError> {
    let report = report
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()?;
    Some(match report {
        HostKeyReport::Unknown {
            key_type,
            fingerprint,
        } => ActionError::UnknownHostKey {
            op,
            host: target.host.clone(),
            key_type,
            fingerprint,
        },
        HostKeyReport::Mismatch {
            key_type,
            fingerprint,
        } => ActionError::HostKeyMismatch {
            op,
            host: target.host.clone(),
            key_type,
            fingerprint,
        },
    })
}

fn connection_err(
    op: &'static str,
    target: &SshTarget,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ActionError {
    ActionError::Git {
        op,
        name: target.host.clone(),
        source: Box::new(source),
    }
}

fn timeout_err(op: &'static str, target: &SshTarget) -> ActionError {
    ActionError::Git {
        op,
        name: target.host.clone(),
        source: format!(
            "'{}:{}' did not complete the ssh handshake within {}s",
            target.host_name,
            target.port,
            CONNECT_TIMEOUT.as_secs()
        )
        .into(),
    }
}
