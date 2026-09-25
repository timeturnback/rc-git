---
title: "Sync spins forever: a cancel flag nobody could set and a transport with no idle budget"
date: 2026-09-25
category: runtime-errors
module: rc-git-guard
problem_type: runtime_error
component: infrastructure
symptoms:
  - "The toolbar's sync button is replaced by a spinner that never stops; the button itself is hidden, so there is nothing to click"
  - "Fetch, Pull, Push and Sync are all disabled in the Repository menu while the stuck transfer holds `isBusy`"
  - "No error, no timeout; quitting the app is the only way out"
  - "Reproduces with a dropped VPN, a sleeping laptop, or any peer that accepts the connection and then goes quiet"
root_cause: api_misuse
resolution_type: code_fix
severity: high
related_components:
  - rc-git-ffi
  - RcGitKit
  - RcGitApp
tags: [gix, russh, tokio, cancellation, timeout, ffi, blocking-io, sync-io-bridge]
---

# Sync spins forever: a cancel flag nobody could set and a transport with no idle budget

## Problem

`rc-git-guard`'s remote operations already took a `cancel: &AtomicBool`, and gix polls it
while receiving a pack. Nothing could ever set it: the FFI built a fresh
`AtomicBool::new(false)` inside each call. Underneath that, the ssh transport's blocking
reads had no time limit at all. A transfer that stopped moving had exactly two outcomes,
success and forever.

## Symptoms

- The toolbar spinner runs indefinitely. `showSyncSpinner` set `syncButton?.isHidden = busy`,
  so the one control that could have stopped it was off screen.
- Every sync menu item validated against `!syncModel.isBusy` and stayed disabled.
- Nothing timed out: no error was ever presented, because no step ever returned.
- Restarting the app was the only exit, which also threw away the evidence.

## What Didn't Work

- **Reading the cancel parameter as if it were wired up.** `fetch_upstream` and
  `push_upstream` take a cancel flag and check it at every step boundary; `receive()` gets it
  as gix's `should_interrupt`. All of that was live code. The break was one layer up, at
  `crates/rc-git-ffi/src/lib.rs`, where each function created its own flag and dropped it on
  return. A parameter that exists is not a cancel path.
- **Expecting git config to supply the timeouts.** gix reads `http.lowSpeedLimit`,
  `http.lowSpeedTime` and `gitoxide.http.connectTimeout` into its curl options — but
  repositories here open with `gix::open::Options::isolated()`, which reads no global or
  system config and no environment. Those keys can only come from code.
- **Passing `tokio::time::timeout(...)` to `block_on`.** The obvious spelling panics:

  ```
  there is no reactor running, must be called from the context of a Tokio 1.x runtime
  ```

  The arguments are evaluated first, so the `Sleep` is constructed before `block_on` enters
  the runtime context. The pre-existing `SshWrite::drop` had the same latent shape.

## Solution

**One flag that outlives the call.** `Cancel` wraps `Arc<AtomicBool>` and replaces the bare
borrow on `fetch_upstream`, `push_upstream`, `sync` and `build_pack`, so the ssh transport can
hold a share of it after the function that built the transport has returned
(`crates/rc-git-guard/src/actions/remote.rs`).

**Poll instead of block.** `Watch::drive` in
`crates/rc-git-guard/src/actions/remote/ssh/session.rs` replaces `SyncIoBridge`'s plain
blocking read. Each channel read and write is polled in short slices:

```rust
let started = Instant::now();
let mut operation = pin!(operation);
loop {
    if self.cancel.load(Ordering::SeqCst) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, ...));
    }
    // The timer is built inside the async block on purpose: constructing a
    // `Sleep` needs the runtime context, which `block_on` only enters once it
    // has the future in hand.
    let waited = handle
        .block_on(async { tokio::time::timeout(CANCEL_POLL, operation.as_mut()).await });
    match waited {
        Ok(result) => return result,
        Err(_elapsed) if started.elapsed() >= self.idle => return Err(timed_out),
        Err(_elapsed) => {}
    }
}
```

The future is pinned once and re-polled; an elapsed slice drops only the `Timeout` wrapper,
so nothing the peer already sent is lost. `CANCEL_POLL` is 200 ms, `IDLE_TIMEOUT` 120 s, and
the clock restarts on every call, so it measures the gap between bytes rather than the length
of the transfer.

**Timeouts in code for curl.** `http_timeouts` sets `connect_timeout` to 30 s and
`low_speed_limit_bytes_per_second: 1` with `low_speed_time_seconds: 120` — "fail when fewer
than one byte moves for two minutes", which never punishes a slow link. Fetch applies it with
`connection.set_transport_options(...)`, seeded from `repo.transport_options()` so a
repository's own `http.proxy` survives; push, which builds its transport by hand, applies it
with `transport.configure(&options)` because `connect::Options` carries no HTTP settings.

**A handle across the ABI.** `RcGitCancel` (opaque, `rcgit_cancel_new` / `_signal` / `_free`)
is the one FFI handle meant to be touched from a second thread — the thread inside a transfer
cannot ask itself to stop. `RCGIT_CANCELLED` (-5) keeps a cancel from being reported as a
failure. Swift's `CancelToken` frees it in `deinit`, and `SyncModel` mints one per workflow so
a signalled token is never reused.

**A control that stays on screen.** While busy the toolbar button becomes `stop.circle` wired
to `cancelSync`, with the spinner beside it rather than in place of it, plus
Repository ▸ Cancel Sync on ⌘.

## Why This Works

A cancel has to reach the thread that is blocked, and that thread cannot check anything while
it sits in a blocking `read`. Slicing the wait is what turns an un-interruptible call into an
interruptible one; the `Arc` is what lets the flag reach it. The idle budget covers the case
the user is not watching — a dead TCP connection produces no bytes and no error, so only a
clock can end it. 120 s is safe because a git server sends keepalives every few seconds while
it thinks, so that much total silence means the connection is gone, not that the repository is
large.

The costs are nil on the happy path: one relaxed atomic load per I/O chunk (gix already does
this while receiving a pack), and a timer only while the socket is silent.

## Prevention

- **Nothing may have only "success" and "forever" as outcomes.** The same rule the updater
  learned at the process level applies at the socket level. Every blocking call in
  `actions/remote/` gets a cancel poll and an idle budget; `.claude/rules/guard.md` now says
  so.
- **A cancel parameter is not a cancel path.** Trace it to something a second thread can set.
  A flag constructed inside the callee is dead code that reads as a feature.
- **An isolated open means configuration comes from code.** `Options::isolated()` is a safety
  property here, and the cost is that any gix behavior normally tuned through git config —
  timeouts included — has to be set explicitly.
- **Build a tokio timer inside the `async` block, never as an argument to `block_on`.** A
  `Sleep` needs the runtime context that `block_on` has not entered yet.
- **Verify a cancellation test by breaking it.** Disabling the flag check in `Watch::drive`
  makes `a_cancel_raised_while_blocked_wakes_the_operation` hang to the harness cap and fail;
  a cancellation test that still passes with the check removed is not testing anything. The
  idle budget is a `Watch` field rather than the constant so the same test can measure it in
  milliseconds.

## Related

- [Rebuild and Relaunch spins forever at preflight](unbounded-preflight-probe-spins-with-no-step-log.md)
  — the same class of bug one layer up: an unbounded out-of-process wait with no way to see or
  stop it. That one bounded a probe; this one bounds a socket.
- [Push over HTTPS with gix that has no push](../architecture-patterns/hand-rolled-git-push-over-gix-https.md)
  — the push transport this work threads `Cancel` and the curl timeouts through.
