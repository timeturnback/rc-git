//! Bridging russh's async channel to the blocking `Read`/`Write` pair
//! `gix_transport`'s git `Connection` needs. A one-worker tokio runtime lives exactly
//! as long as the transport; both halves hold it through an `Arc` so it shuts down
//! once, after the last half is dropped, with a timeout so an FFI thread never hangs.

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use russh::ChannelStream;
use russh::client::{Handle, Msg};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::runtime::Runtime;
use tokio_util::io::SyncIoBridge;

use crate::actions::{ActionError, ActionResult};

use super::hostkey::HostKeyHandler;

/// Builds the runtime every ssh transport runs on.
///
/// # Errors
/// [`ActionError::Git`] when tokio cannot start its worker thread.
pub(super) fn build_runtime(op: &'static str) -> ActionResult<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_io()
        .enable_time()
        .build()
        .map_err(|source| ActionError::Git {
            op,
            name: "ssh".to_owned(),
            source: Box::new(source),
        })
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
            runtime.shutdown_timeout(Duration::from_secs(5));
        }
    }
}

/// Blocking reader over the channel's stdout. Its inner half is dropped inside the
/// runtime's context: russh's channel types spawn a close notification on drop, which
/// panics without a reactor.
pub(super) struct SshRead {
    inner: Option<SyncIoBridge<ReadHalf<ChannelStream<Msg>>>>,
    handle: tokio::runtime::Handle,
    _hold: Arc<SessionHold>,
}

impl SshRead {
    pub(super) fn new(
        half: ReadHalf<ChannelStream<Msg>>,
        handle: tokio::runtime::Handle,
        hold: &Arc<SessionHold>,
    ) -> Self {
        Self {
            inner: Some(SyncIoBridge::new_with_handle(half, handle.clone())),
            handle,
            _hold: Arc::clone(hold),
        }
    }
}

impl Read for SshRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.inner.as_mut() {
            Some(inner) => inner.read(buf),
            None => Ok(0),
        }
    }
}

impl Drop for SshRead {
    fn drop(&mut self) {
        let _context = self.handle.enter();
        drop(self.inner.take());
    }
}

/// Blocking writer over the channel's stdin. Dropping it sends channel EOF so the
/// remote `git-receive-pack`/`git-upload-pack` sees end-of-input — `SyncIoBridge`
/// does not shut the stream down on drop by itself. Like [`SshRead`], the inner half
/// is dropped inside the runtime's context.
pub(super) struct SshWrite {
    inner: Option<SyncIoBridge<WriteHalf<ChannelStream<Msg>>>>,
    handle: tokio::runtime::Handle,
    _hold: Arc<SessionHold>,
}

impl SshWrite {
    pub(super) fn new(
        half: WriteHalf<ChannelStream<Msg>>,
        handle: tokio::runtime::Handle,
        hold: &Arc<SessionHold>,
    ) -> Self {
        Self {
            inner: Some(SyncIoBridge::new_with_handle(half, handle.clone())),
            handle,
            _hold: Arc::clone(hold),
        }
    }
}

impl Write for SshWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.inner.as_mut() {
            Some(inner) => inner.write(buf),
            None => Err(std::io::Error::other("the ssh channel is closed")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.inner.as_mut() {
            Some(inner) => inner.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for SshWrite {
    fn drop(&mut self) {
        let _context = self.handle.enter();
        if let Some(mut inner) = self.inner.take() {
            inner.shutdown().ok();
            drop(inner);
        }
    }
}
