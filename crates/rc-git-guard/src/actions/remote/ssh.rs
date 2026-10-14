//! The ssh transport: an in-process SSH client (russh) running `git-upload-pack` /
//! `git-receive-pack` on the server and speaking the git wire protocol over the
//! channel. No child process is ever spawned — the same invariant the HTTPS path
//! keeps. `known_hosts` is consulted read-only; trust decisions live with the caller.

use std::cell::Cell;
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
mod dial;
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

/// The git service a connection runs on the server, and the protocol version it asks for.
#[derive(Debug, Clone, Copy)]
pub(super) struct Exec {
    /// `git-upload-pack` for a fetch, `git-receive-pack` for a push.
    pub(super) service: Service,
    /// The protocol version the transport speaks; V2 is requested only for upload-pack.
    pub(super) version: Protocol,
}

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
    exec: Exec,
    cancel: &Cancel,
    op: &'static str,
) -> ActionResult<BoxedTransport> {
    let target = target::resolve(url, auth.ssh_dir, op)?;
    let command = target::quoted_command(exec.service, url.path.as_ref(), op)?;
    bail_if_cancelled(cancel, op)?;
    let request = Request {
        target: &target,
        auth,
        command: &command,
        exec,
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
        exec.version,
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
    exec: Exec,
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
    let phase = Cell::new(Phase::Handshake);
    // Entered for the whole call so whatever the abandoned future or a refused session
    // drops (russh spawns close notifications on drop) still finds a reactor.
    let _context = runtime.enter();
    let connected = drive(
        runtime,
        connect_and_exec(request, handler, passphrase, &phase),
        request,
        &phase,
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
/// dial. The future is pinned once, so an elapsed slice drops only the timer. A timeout
/// names the `phase` the connect had reached.
fn drive(
    runtime: &Runtime,
    connecting: impl Future<Output = ActionResult<Connected>>,
    request: &Request<'_>,
    phase: &Cell<Phase>,
) -> ActionResult<Connected> {
    let budget = request.cancel.connect();
    let deadline = Instant::now() + budget;
    let mut connecting = pin!(connecting);
    loop {
        bail_if_cancelled(request.cancel, request.op)?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out(request, phase.get(), budget));
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

/// How far a connect got, so a timeout tells a network fault from a stalled login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Dialing, the ssh handshake and the host-key check.
    Handshake,
    /// Authenticating, which includes waiting on an ssh-agent to sign.
    Authenticate,
    /// Opening the session channel and starting the git service.
    Exec,
}

/// The error for a connect that outlived its budget in `phase`.
fn timed_out(request: &Request<'_>, phase: Phase, budget: Duration) -> ActionError {
    let target = request.target;
    ActionError::git(
        request.op,
        &target.host,
        timeout_message(phase, &target.host_name, target.port, budget),
    )
}

/// The message for a connect to `host_name:port` that ran out of `budget` in `phase`.
fn timeout_message(phase: Phase, host_name: &str, port: u16, budget: Duration) -> String {
    match phase {
        Phase::Handshake => {
            format!("'{host_name}:{port}' did not complete the ssh handshake within {budget:?}")
        }
        Phase::Authenticate => format!(
            "'{host_name}:{port}' accepted the connection, but authentication did not finish \
             within {budget:?} (an ssh-agent waiting for approval, or a key the server is slow \
             to check)"
        ),
        Phase::Exec => format!(
            "'{host_name}:{port}' accepted the login, but did not start the git service \
             within {budget:?}"
        ),
    }
}

/// Dial (see [`dial`]), verify the host key, authenticate, open a session channel, and exec the git
/// service on it. Runs on the transport's private runtime; [`drive`] bounds it.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned remote-I/O path; see docs/safety.md"
)]
async fn connect_and_exec(
    request: &Request<'_>,
    handler: HostKeyHandler,
    passphrase: Option<&str>,
    phase: &Cell<Phase>,
) -> ActionResult<Connected> {
    let Request {
        target,
        command,
        exec,
        op,
        ..
    } = *request;
    let config = Arc::new(russh::client::Config::default());
    let socket = dial::dial(target, request.cancel.connect(), op).await?;
    if config.nodelay {
        socket
            .set_nodelay(true)
            .map_err(git_err(op, &target.host))?;
    }
    let mut client = russh::client::connect_stream(config, socket, handler)
        .await
        .map_err(git_err(op, &target.host))?;
    phase.set(Phase::Authenticate);
    auth::authenticate(&mut client, target, passphrase, op).await?;
    phase.set(Phase::Exec);
    let channel = client
        .channel_open_session()
        .await
        .map_err(git_err(op, &target.host))?;
    if exec.service == Service::UploadPack && exec.version == Protocol::V2 {
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
            approved,
        } => ActionError::HostKeyMismatch {
            op,
            host: target.host.clone(),
            key_type,
            fingerprint,
            approved,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_auth_timeout_names_authentication() {
        let budget = Duration::from_secs(30);
        let handshake = timeout_message(Phase::Handshake, "example.com", 22, budget);
        let auth = timeout_message(Phase::Authenticate, "example.com", 22, budget);
        let exec = timeout_message(Phase::Exec, "example.com", 22, budget);
        assert!(handshake.contains("ssh handshake"), "{handshake}");
        assert!(auth.contains("authentication"), "{auth}");
        assert!(auth.contains("ssh-agent"), "{auth}");
        assert!(!auth.contains("handshake"), "{auth}");
        assert!(exec.contains("git service"), "{exec}");
        for message in [&handshake, &auth, &exec] {
            assert!(message.contains("'example.com:22'"), "{message}");
            assert!(message.contains("30s"), "{message}");
        }
    }
}
