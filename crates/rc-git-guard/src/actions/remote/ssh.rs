//! The ssh transport: an in-process SSH client (russh) running `git-upload-pack` /
//! `git-receive-pack` on the server and speaking the git wire protocol over the
//! channel. No child process is ever spawned — the same invariant the HTTPS path
//! keeps. `known_hosts` is consulted read-only; trust decisions live with the caller.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gix::protocol::transport::client::git::ConnectMode;
use gix::protocol::transport::{Protocol, Service};
use russh::client::{Handle, Msg};
use tokio::runtime::Runtime;

use crate::actions::{ActionError, ActionResult, git_err};

use super::{CANCEL_POLL, Cancel, RemoteAuth, bail_if_cancelled};

mod auth;
mod hostkey;
mod output;
mod session;
mod target;

use hostkey::{HostKeyHandler, HostKeyReport, ReportSlot};
pub(super) use output::ServerRefusal;
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
/// channel is ready, [`ActionError::Git`] when the connect budget runs out and for every
/// other failure.
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
    let request = Request {
        target: &target,
        auth,
        command: &command,
        service,
        desired_version,
        cancel,
        op,
    };
    let runtime = session::build_runtime(op)?;
    let (runtime, (client, channel)) =
        session::connect_or_abandon(runtime, |runtime| connect(runtime, &request))?;
    let (read_half, write_half) = channel.split();
    let writer: session::ChannelWriter = Box::pin(write_half.make_writer());
    let handle = runtime.handle().clone();
    let hold = SessionHold::new(client, runtime);
    let watch = session::Watch::new(cancel, op);
    let connection = gix::protocol::transport::client::git::blocking_io::Connection::new(
        SshRead::new(
            (read_half, write_half),
            handle.clone(),
            &hold,
            watch.clone(),
        ),
        SshWrite::new(writer, handle, &hold, watch),
        desired_version,
        url.path.clone(),
        None::<(String, _)>,
        ConnectMode::Process,
        false,
    )
    .custom_url(Some(super::redacted(url).into()));
    Ok(Box::new(connection))
}

/// Everything one connect attempt needs, borrowed from [`transport`].
struct Request<'a> {
    target: &'a SshTarget,
    auth: RemoteAuth<'a>,
    command: &'a str,
    service: Service,
    desired_version: Protocol,
    cancel: &'a Cancel,
    op: &'static str,
}

/// The connected session and its exec'd channel.
type Connected = (Handle<HostKeyHandler>, russh::Channel<Msg>);

/// Runs [`connect_and_exec`] on `runtime` under the cancel flag and the connect budget,
/// and maps a failure to the error the caller should see: a cancel wins over whatever
/// else went wrong, then a host-key refusal the handler recorded.
fn connect(runtime: &Runtime, request: &Request<'_>) -> ActionResult<Connected> {
    let Request {
        target,
        auth,
        cancel,
        op,
        ..
    } = *request;
    let report: ReportSlot = Arc::new(Mutex::new(None));
    let handler = HostKeyHandler::new(target, auth.accepted_hostkey, Arc::clone(&report));
    let passphrase = auth.creds.map(|creds| creds.token.as_str());
    // Entered for the whole call so whatever the abandoned future or a refused session
    // drops (russh spawns close notifications on drop) still finds a reactor.
    let _context = runtime.enter();
    let connected = drive(
        runtime,
        connect_and_exec(request, handler, passphrase),
        request,
    );
    match connected {
        Ok(pair) => {
            bail_if_cancelled(cancel, op)?;
            Ok(pair)
        }
        Err(err) => {
            bail_if_cancelled(cancel, op)?;
            Err(typed_refusal(&report, target, op).unwrap_or(err))
        }
    }
}

/// Polls `connecting` on `runtime` until it finishes, waking every [`CANCEL_POLL`] to
/// look at the cancel flag. Dialing, the handshake, authentication and the session
/// channel share one deadline, the request's connect budget, measured from before the
/// dial. The future is pinned once, so an elapsed slice drops only the timer.
fn drive(
    runtime: &Runtime,
    connecting: impl Future<Output = ActionResult<Connected>>,
    request: &Request<'_>,
) -> ActionResult<Connected> {
    let budget = request.cancel.connect();
    let deadline = Instant::now() + budget;
    let mut connecting = pin!(connecting);
    loop {
        bail_if_cancelled(request.cancel, request.op)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out(request, budget));
        }
        let slice = left.min(CANCEL_POLL);
        // The timer is built inside the async block: a `Sleep` needs the runtime context
        // that `block_on` enters only once it has the future in hand.
        let waited =
            runtime.block_on(async { tokio::time::timeout(slice, connecting.as_mut()).await });
        if let Ok(result) = waited {
            return result;
        }
    }
}

/// The error for a connect that outlived its budget.
fn timed_out(request: &Request<'_>, budget: Duration) -> ActionError {
    let target = request.target;
    ActionError::git(
        request.op,
        &target.host,
        format!(
            "'{}:{}' did not complete the ssh handshake within {budget:?}",
            target.host_name, target.port
        ),
    )
}

/// Dial, verify the host key, authenticate, open a session channel, and exec the git
/// service on it. Runs on the transport's private runtime; [`drive`] bounds it.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
)]
async fn connect_and_exec(
    request: &Request<'_>,
    handler: HostKeyHandler,
    passphrase: Option<&str>,
) -> ActionResult<Connected> {
    let Request {
        target,
        command,
        service,
        desired_version,
        op,
        ..
    } = *request;
    let config = Arc::new(russh::client::Config::default());
    let address = (target.host_name.as_str(), target.port);
    let mut client = russh::client::connect(config, address, handler)
        .await
        .map_err(git_err(op, &target.host))?;
    auth::authenticate(&mut client, target, passphrase, op).await?;
    let channel = client
        .channel_open_session()
        .await
        .map_err(git_err(op, &target.host))?;
    if service == Service::UploadPack && desired_version == Protocol::V2 {
        // Best effort: a server that ignores the env just advertises V0/V1, which the
        // handshake accepts as a downgrade.
        channel
            .set_env(false, "GIT_PROTOCOL", "version=2")
            .await
            .map_err(git_err(op, &target.host))?;
    }
    channel
        .exec(false, command)
        .await
        .map_err(git_err(op, &target.host))?;
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
