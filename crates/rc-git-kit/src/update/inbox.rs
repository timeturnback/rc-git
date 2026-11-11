//! Carries build output from the build's reading thread to the main thread: each batch is
//! tagged with the attempt that printed it, so a batch a cancelled build prints late is
//! recognized as stale when it lands.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

/// One read of build output and the attempt whose build printed it.
pub(super) type Batch = (u64, Vec<String>);

#[derive(Default)]
struct State {
    queue: VecDeque<Batch>,
    waker: Option<Waker>,
}

/// A queue of output batches with one awaiting reader.
#[derive(Default)]
pub(super) struct OutputInbox {
    state: Mutex<State>,
}

impl OutputInbox {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `lines` from `attempt` and wakes the reader. Safe from any thread.
    pub(super) fn push(&self, attempt: u64, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        let waker = {
            let mut state = self.lock();
            state.queue.push_back((attempt, lines));
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Every batch queued now, oldest first.
    pub(super) fn take_all(&self) -> Vec<Batch> {
        self.lock().queue.drain(..).collect()
    }

    /// Resolves to the oldest queued batch once there is one.
    pub(super) fn next(self: &Arc<Self>) -> NextBatch {
        NextBatch {
            inbox: Arc::clone(self),
        }
    }
}

/// The future [`OutputInbox::next`] returns.
pub(super) struct NextBatch {
    inbox: Arc<OutputInbox>,
}

impl Future for NextBatch {
    type Output = Batch;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Batch> {
        let mut state = self.inbox.lock();
        if let Some(batch) = state.queue.pop_front() {
            return Poll::Ready(batch);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
