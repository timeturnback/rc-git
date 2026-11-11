//! A channel that carries one value from any thread to one awaiting future. [`crate::blocking`]
//! hands a worker thread's result back through it, and a test hands a gated answer to a
//! provider it holds open.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

struct State<T> {
    value: Option<T>,
    waker: Option<Waker>,
    closed: bool,
}

type Shared<T> = Arc<Mutex<State<T>>>;

fn lock<T>(shared: &Shared<T>) -> MutexGuard<'_, State<T>> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sending half. Dropping it unsent resolves the receiver to `None`.
pub struct Sender<T> {
    shared: Shared<T>,
}

/// The receiving half: a future that resolves to the sent value, or to `None` once the
/// sender is gone without sending.
pub struct Receiver<T> {
    shared: Shared<T>,
}

/// A connected sender and receiver.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Mutex::new(State {
        value: None,
        waker: None,
        closed: false,
    }));
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

impl<T> Sender<T> {
    /// Hands `value` to the receiver and wakes it.
    pub fn send(self, value: T) {
        let waker = {
            let mut state = lock(&self.shared);
            state.value = Some(value);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut state = lock(&self.shared);
            state.closed = true;
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Future for Receiver<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = lock(&self.shared);
        if let Some(value) = state.value.take() {
            return Poll::Ready(Some(value));
        }
        if state.closed {
            return Poll::Ready(None);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
