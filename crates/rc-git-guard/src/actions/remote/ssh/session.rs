//! Bridging russh's async channel to the blocking `Read`/`Write` pair
//! `gix_transport`'s git `Connection` needs. A one-worker tokio runtime lives exactly
//! as long as the transport; both halves hold it through an `Arc` so it shuts down
//! once, after the last half is dropped, with a timeout so an FFI thread never hangs.
//!
//! Every read and write is driven through [`Watch`] rather than blocking outright:
//! the channel is the one place an ssh transfer can stall forever, so the halves wake
//! on a short interval to look at the cancel flag and at how long the peer has been
//! silent.

use std::future::Future;
use std::io::{self, Read, Write};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use russh::client::{Handle, Msg};
use russh::{ChannelReadHalf, ChannelWriteHalf};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::runtime::Runtime;

use crate::actions::remote::{CANCEL_POLL, Cancel};
use crate::actions::{ActionResult, git_err};

use super::hostkey::HostKeyHandler;
use super::output::ChannelReader;

/// How long the closing channel EOF, the server's exit status after its EOF, and the
/// channel close may each take before the transport gives up on them.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// The channel's stdin writer, boxed because russh only names it as `impl AsyncWrite`.
pub(super) type ChannelWriter = Pin<Box<dyn AsyncWrite + Send>>;

/// Builds the runtime every ssh transport runs on.
///
/// # Errors
/// [`ActionError::Git`](crate::actions::ActionError::Git) when tokio cannot start its worker
/// thread.
pub(super) fn build_runtime(op: &'static str) -> ActionResult<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_io()
        .enable_time()
        .build()
        .map_err(git_err(op, "ssh"))
}

/// Runs `connect` against `runtime` and hands the runtime back with its result. On
/// failure the runtime is shut down without joining its blocking pool: a host name is
/// resolved there, and a stalled `getaddrinfo` would otherwise hold the caller until the
/// OS resolver gives up, long after a cancel or the connect budget said to stop.
///
/// # Errors
/// Whatever `connect` returns.
pub(super) fn connect_or_abandon<T>(
    runtime: Runtime,
    connect: impl FnOnce(&Runtime) -> ActionResult<T>,
) -> ActionResult<(Runtime, T)> {
    match connect(&runtime) {
        Ok(value) => Ok((runtime, value)),
        Err(err) => {
            runtime.shutdown_background();
            Err(err)
        }
    }
}

/// The cancel flag and the idle budget every channel read and write is measured
/// against. Both halves hold a clone, so a cancel raised after the transport was built
/// still lands.
#[derive(Clone)]
pub(super) struct Watch {
    cancel: Arc<AtomicBool>,
    op: &'static str,
    /// How long one read or write may make no progress, taken from the [`Cancel`] so
    /// both transports share one budget and tests can shrink it.
    idle: Duration,
}

impl Watch {
    pub(super) fn new(cancel: &Cancel, op: &'static str) -> Self {
        Self {
            cancel: cancel.share(),
            op,
            idle: cancel.idle(),
        }
    }

    /// Polls `operation` on the runtime until it finishes, waking every
    /// [`CANCEL_POLL`] to look at the cancel flag.
    ///
    /// Returns an [`io::ErrorKind::Other`] error once the flag is set, and
    /// [`io::ErrorKind::TimedOut`] once the peer has been silent for the idle
    /// budget. Never [`io::ErrorKind::Interrupted`]: std's `read_exact`, `write_all`
    /// and `read_to_end` retry that kind, so a cancel would spin forever. The clock starts at each call, so it measures the gap between
    /// bytes rather than the length of the transfer: a slow link is never punished,
    /// only a dead one.
    ///
    /// The future is pinned once and re-polled; the elapsed timeout drops only the
    /// wrapper, so nothing the peer already sent is lost.
    fn drive<T>(
        &self,
        handle: &tokio::runtime::Handle,
        operation: impl Future<Output = io::Result<T>>,
    ) -> io::Result<T> {
        let started = Instant::now();
        let mut operation = pin!(operation);
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err(io::Error::other(format!("{}: cancelled", self.op)));
            }
            // The timer is built inside the async block on purpose: constructing a
            // `Sleep` needs the runtime context, which `block_on` only enters once it
            // has the future in hand.
            let waited = handle
                .block_on(async { tokio::time::timeout(CANCEL_POLL, operation.as_mut()).await });
            match waited {
                Ok(result) => return result,
                Err(_elapsed) if started.elapsed() >= self.idle => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{}: the ssh channel moved no data for {}s",
                            self.op,
                            self.idle.as_secs()
                        ),
                    ));
                }
                Err(_elapsed) => {}
            }
        }
    }
}

/// Keeps the ssh session and its runtime alive for as long as either I/O half exists.
pub(super) struct SessionHold {
    client: Option<Handle<HostKeyHandler>>,
    runtime: Option<Runtime>,
}

impl SessionHold {
    pub(super) fn new(client: Handle<HostKeyHandler>, runtime: Runtime) -> Arc<Self> {
        Arc::new(Self {
            client: Some(client),
            runtime: Some(runtime),
        })
    }
}

impl Drop for SessionHold {
    fn drop(&mut self) {
        drop(self.client.take());
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);
        }
    }
}

/// Blocking reader over the channel's stdout; the server's stderr and exit status come
/// back through it as the error that ends the stream (see [`ChannelReader`]). It owns
/// the channel's write half only to send the channel close when it is dropped, inside
/// the runtime, as russh's own stream did.
pub(super) struct SshRead {
    inner: Option<ChannelReader>,
    closer: Option<ChannelWriteHalf<Msg>>,
    handle: tokio::runtime::Handle,
    watch: Watch,
    _hold: Arc<SessionHold>,
}

impl SshRead {
    pub(super) fn new(
        (read_half, write_half): (ChannelReadHalf, ChannelWriteHalf<Msg>),
        handle: tokio::runtime::Handle,
        hold: &Arc<SessionHold>,
        watch: Watch,
    ) -> Self {
        Self {
            inner: Some(ChannelReader::new(read_half, SHUTDOWN_TIMEOUT)),
            closer: Some(write_half),
            handle,
            watch,
            _hold: Arc::clone(hold),
        }
    }
}

impl Read for SshRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.as_mut() {
            Some(inner) => self.watch.drive(&self.handle, inner.read(buf)),
            None => Ok(0),
        }
    }
}

impl Drop for SshRead {
    fn drop(&mut self) {
        drop(self.inner.take());
        let Some(closer) = self.closer.take() else {
            return;
        };
        // A session that is already gone refuses the close; there is nothing left to
        // tell it, so that result is dropped.
        let closed = self
            .handle
            .block_on(async { tokio::time::timeout(SHUTDOWN_TIMEOUT, closer.close()).await });
        drop(closed);
        let _context = self.handle.enter();
        drop(closer);
    }
}

/// Blocking writer over the channel's stdin. Dropping it sends channel EOF so the
/// remote `git-receive-pack`/`git-upload-pack` sees end-of-input — dropping the half
/// does not shut the stream down by itself. Like [`SshRead`], the inner half is
/// dropped inside the runtime's context.
pub(super) struct SshWrite {
    inner: Option<ChannelWriter>,
    handle: tokio::runtime::Handle,
    watch: Watch,
    _hold: Arc<SessionHold>,
}

impl SshWrite {
    pub(super) fn new(
        half: ChannelWriter,
        handle: tokio::runtime::Handle,
        hold: &Arc<SessionHold>,
        watch: Watch,
    ) -> Self {
        Self {
            inner: Some(half),
            handle,
            watch,
            _hold: Arc::clone(hold),
        }
    }
}

impl Write for SshWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.inner.as_mut() {
            Some(inner) => self.watch.drive(&self.handle, inner.write(buf)),
            None => Err(io::Error::other("the ssh channel is closed")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.inner.as_mut() {
            Some(inner) => self.watch.drive(&self.handle, inner.flush()),
            None => Ok(()),
        }
    }
}

impl Drop for SshWrite {
    fn drop(&mut self) {
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        // Bounded on its own rather than through `Watch`: a cancel is what usually
        // brings us here, and the EOF still has to be attempted so the server stops
        // waiting for input.
        let shutdown = self
            .handle
            .block_on(async { tokio::time::timeout(SHUTDOWN_TIMEOUT, inner.shutdown()).await });
        drop(shutdown);
        let _context = self.handle.enter();
        drop(inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionError;
    use crate::actions::remote::IDLE_TIMEOUT;

    fn watch(cancel: &Cancel, idle: Duration) -> Watch {
        Watch {
            idle,
            ..Watch::new(cancel, "test")
        }
    }

    #[test]
    fn an_early_return_does_not_wait_on_blocking_work() {
        let runtime = build_runtime("test").expect("runtime");
        let (running, started_blocking) = std::sync::mpsc::channel();
        drop(runtime.spawn_blocking(move || {
            running.send(()).expect("the test is waiting");
            std::thread::sleep(Duration::from_secs(5));
        }));
        started_blocking
            .recv_timeout(Duration::from_secs(1))
            .expect("the blocking task starts");
        let started = Instant::now();
        let result = connect_or_abandon(runtime, |_| -> ActionResult<()> {
            Err(ActionError::Cancelled { op: "fetch" })
        });
        assert!(matches!(
            result,
            Err(ActionError::Cancelled { op: "fetch" })
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_finished_operation_returns_its_value() {
        let runtime = build_runtime("test").expect("runtime");
        let watch = watch(&Cancel::new(), IDLE_TIMEOUT);
        let read = watch.drive(runtime.handle(), std::future::ready(Ok(7_usize)));
        assert_eq!(read.expect("value"), 7);
    }

    #[test]
    fn a_cancel_raised_while_blocked_wakes_the_operation() {
        let runtime = build_runtime("test").expect("runtime");
        let cancel = Cancel::new();
        let watch = watch(&cancel, IDLE_TIMEOUT);
        let raiser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel.signal();
        });
        let blocked = watch.drive(
            runtime.handle(),
            std::future::pending::<io::Result<usize>>(),
        );
        raiser.join().expect("raiser");
        let error = blocked.expect_err("cancelled");
        assert!(
            !matches!(error.kind(), io::ErrorKind::Interrupted),
            "{error:?}"
        );
        assert!(error.to_string().contains("cancelled"), "{error}");
    }

    /// A reader and writer that block forever unless the watch ends them, as the ssh
    /// halves do on a silent peer.
    struct Blocked {
        runtime: Runtime,
        watch: Watch,
    }

    impl Read for Blocked {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            self.watch.drive(
                self.runtime.handle(),
                std::future::pending::<io::Result<usize>>(),
            )
        }
    }

    impl Write for Blocked {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            self.watch.drive(
                self.runtime.handle(),
                std::future::pending::<io::Result<usize>>(),
            )
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_cancel_ends_the_std_io_helpers_that_retry_interrupted_errors() {
        let cancel = Cancel::new();
        let mut blocked = Blocked {
            runtime: build_runtime("test").expect("runtime"),
            watch: watch(&cancel, IDLE_TIMEOUT),
        };
        cancel.signal();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read = blocked.read_exact(&mut [0_u8; 4]);
            let read_to_end = blocked.read_to_end(&mut Vec::new());
            let written = blocked.write_all(b"pack");
            drop(done.send((read, read_to_end, written)));
        });
        let (read, read_to_end, written) = finished
            .recv_timeout(Duration::from_secs(1))
            .expect("every helper returns within a second of the cancel");
        for result in [read, read_to_end.map(drop), written] {
            let error = result.expect_err("cancelled");
            assert!(error.to_string().contains("cancelled"), "{error}");
        }
    }

    #[test]
    fn a_silent_peer_ends_the_operation_once_the_idle_budget_is_spent() {
        let runtime = build_runtime("test").expect("runtime");
        let watch = watch(&Cancel::new(), Duration::from_millis(10));
        let blocked = watch.drive(
            runtime.handle(),
            std::future::pending::<io::Result<usize>>(),
        );
        let error = blocked.expect_err("timed out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("moved no data"), "{error}");
    }

    #[test]
    fn a_silent_peer_is_timed_across_several_polls() {
        let runtime = build_runtime("test").expect("runtime");
        let watch = watch(&Cancel::new(), CANCEL_POLL * 3);
        let started = Instant::now();
        let blocked = watch.drive(
            runtime.handle(),
            std::future::pending::<io::Result<usize>>(),
        );
        let elapsed = started.elapsed();
        let error = blocked.expect_err("timed out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            elapsed >= CANCEL_POLL * 3 && elapsed < CANCEL_POLL * 5,
            "took {elapsed:?}"
        );
    }

    #[test]
    fn an_operation_pending_at_a_poll_tick_still_returns_its_value() {
        let runtime = build_runtime("test").expect("runtime");
        let watch = watch(&Cancel::new(), Duration::from_secs(5));
        let started = Instant::now();
        let read = watch.drive(runtime.handle(), async {
            tokio::time::sleep(CANCEL_POLL + CANCEL_POLL / 2).await;
            Ok(7_usize)
        });
        let elapsed = started.elapsed();
        assert_eq!(read.expect("value"), 7);
        assert!(elapsed > CANCEL_POLL, "took {elapsed:?}");
    }
}
