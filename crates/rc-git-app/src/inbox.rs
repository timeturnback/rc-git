//! A queue another thread pushes into and one UI task drains: the system's open-URL callback,
//! which runs with no app context, and the `FSEvents` callbacks, which run on a dispatch
//! queue. A burst of pushes before the task wakes costs one wake-up and one drain, so a
//! `git fetch` that fires the ref watcher several times refreshes once per drain.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

struct State<T> {
    items: Vec<T>,
    waker: Option<Waker>,
}

/// The queue. Cloning shares it.
pub struct Inbox<T> {
    state: Arc<Mutex<State<T>>>,
}

impl<T> Clone for Inbox<T> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

impl<T> Default for Inbox<T> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                items: Vec::new(),
                waker: None,
            })),
        }
    }
}

impl<T> Inbox<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `item` from any thread and wakes the task waiting on the queue.
    pub fn push(&self, item: T) {
        let waker = {
            let mut state = self.lock();
            state.items.push(item);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Everything queued so far, without waiting.
    pub fn take(&self) -> Vec<T> {
        std::mem::take(&mut self.lock().items)
    }

    /// Resolves with everything queued once at least one item is.
    pub fn next(&self) -> Next<T> {
        Next {
            inbox: Self {
                state: Arc::clone(&self.state),
            },
        }
    }
}

/// The future [`Inbox::next`] returns.
pub struct Next<T> {
    inbox: Inbox<T>,
}

impl<T> Future for Next<T> {
    type Output = Vec<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<T>> {
        let mut state = self.inbox.lock();
        if !state.items.is_empty() {
            return Poll::Ready(std::mem::take(&mut state.items));
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    async fn a_burst_is_one_drain(cx: &mut gpui::TestAppContext) {
        let inbox = Inbox::default();
        let sender = inbox.clone();
        let thread = std::thread::spawn(move || {
            for index in 0..5 {
                sender.push(index);
            }
        });
        let _ = thread.join();
        cx.run_until_parked();
        assert_eq!(inbox.next().await, vec![0, 1, 2, 3, 4]);
        assert!(inbox.take().is_empty());
    }
}
