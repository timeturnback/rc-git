//! The blocking helper: work runs off the test thread and its value comes back to the task,
//! and a held call does not stop the test thread from driving others.

use std::sync::Arc;
use std::thread;

use gpui::TestAppContext;
use rc_git_kit::blocking::blocking;
use rc_git_kit::fake::CallGate;
use rc_git_kit::oneshot;

#[gpui::test]
async fn work_runs_on_its_own_thread_and_hands_its_value_back(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let caller = thread::current().id();
    let (value, worker) = blocking(&cx.executor(), move || (6 * 7, thread::current().id())).await;
    assert_eq!(value, 42);
    assert_ne!(
        worker, caller,
        "blocking work never runs on the calling thread"
    );
}

#[gpui::test]
async fn a_held_call_leaves_the_test_thread_free(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let gate = Arc::new(CallGate::new());
    let held = Arc::clone(&gate);
    let slow = blocking(&cx.executor(), move || {
        held.pass();
        "slow"
    });
    assert!(gate.wait_until_arrived());
    assert_eq!(blocking(&cx.executor(), || "fast").await, "fast");
    gate.open();
    assert_eq!(slow.await, "slow");
}

#[gpui::test]
async fn a_dropped_sender_resolves_the_receiver_to_none(cx: &mut TestAppContext) {
    let (sender, receiver) = oneshot::channel::<u8>();
    drop(sender);
    assert_eq!(receiver.await, None);
    let (sender, receiver) = oneshot::channel();
    sender.send(7);
    assert_eq!(receiver.await, Some(7));
    cx.run_until_parked();
}

#[test]
fn an_arrival_before_the_wait_still_counts_once() {
    let gate = CallGate::new();
    gate.open();
    gate.pass();
    assert!(gate.wait_until_arrived());
}
