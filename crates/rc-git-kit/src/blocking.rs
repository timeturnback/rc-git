//! Port of `BlockingWork.swift`: runs blocking work — a call into the core, a guard action, a
//! keychain access — off the UI thread and hands its value back to an awaiting task.
//!
//! Each call gets its own thread, and the [`BackgroundExecutor`] only awaits the hand-back.
//! GPUI's background pool has one thread per core, so blocking work run on it holds a thread
//! no other task can use; enough of it waiting at once starves every background task, the
//! ones that would unblock it included. That is the starvation `BlockingWork`'s overcommitting
//! queue existed to avoid, and a thread per call is the same promise: a blocked call never
//! takes a pool thread. Models coalesce their loads, so the threads in flight stay few.
//!
//! None of these threads is inside a tokio runtime, so a guard remote action, which builds its
//! own runtime, may run here. Never run one inside a runtime: building a runtime there panics.
//!
//! Dropping the returned task stops nobody's thread: the work runs to its end and its value is
//! dropped. Work that must stop early takes a [`crate::guard_bridge::CancelToken`].
//!
//! Under the GPUI test dispatcher the work still runs on its own thread, so a test can hold it
//! at a gate while the test thread drives the models. Such a test calls
//! `cx.executor().allow_parking()` and awaits the task it started.

use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use gpui::{BackgroundExecutor, Task};

use crate::oneshot;

/// Runs `work` on its own thread and resolves to its value. A panic inside `work` resumes in
/// the task that awaits it, as a panic inside a spawned task would. When no thread can be
/// started, `work` runs on `executor` instead.
pub fn blocking<T, F>(executor: &BackgroundExecutor, work: F) -> Task<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let slot = Arc::new(Mutex::new(Some(work)));
    let (sender, receiver) = oneshot::channel();
    let worker_slot = Arc::clone(&slot);
    let started = thread::Builder::new()
        .name("rc-git-blocking".to_owned())
        .spawn(move || {
            let work = worker_slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(work) = work {
                sender.send(panic::catch_unwind(AssertUnwindSafe(work)));
            }
        });
    if started.is_err() {
        let work = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(work) = work {
            return executor.spawn(async move { work() });
        }
    }
    executor.spawn(async move {
        match receiver.await {
            Some(Ok(value)) => value,
            Some(Err(payload)) => panic::resume_unwind(payload),
            None => panic::resume_unwind(Box::new("blocking work ended without a result")),
        }
    })
}
