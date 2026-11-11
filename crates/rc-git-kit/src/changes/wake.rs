//! A flag a watcher thread raises and one model task awaits. Raising it again before the task
//! has woken changes nothing, so a burst of events costs one wake-up: the model's own
//! coalescing then turns that into at most one scan in flight plus one queued.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct State {
    raised: bool,
    waker: Option<Waker>,
}

/// The flag. Cloning shares it.
#[derive(Clone, Default)]
pub(super) struct WakeFlag {
    state: Arc<Mutex<State>>,
}

impl WakeFlag {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Raises the flag from any thread and wakes the task waiting on it.
    pub(super) fn raise(&self) {
        let waker = {
            let mut state = self.lock();
            state.raised = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Resolves once the flag is raised, lowering it again.
    pub(super) fn raised(&self) -> Raised {
        Raised { flag: self.clone() }
    }
}

/// The future [`WakeFlag::raised`] returns.
pub(super) struct Raised {
    flag: WakeFlag,
}

impl Future for Raised {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.flag.lock();
        if state.raised {
            state.raised = false;
            return Poll::Ready(());
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
