---
title: "Rebuild and Relaunch spins forever at preflight: an unbounded out-of-process probe behind a one-line log"
date: 2026-09-21
category: runtime-errors
module: RcGitApp
problem_type: runtime_error
component: frontend
symptoms:
  - "The update window sits at \"Checking the toolchain and the checkout…\" with the spinner running and the button disabled; it never advances or fails"
  - "The log holds one line, `checkout: /Users/…/rc-git`, and nothing after it"
  - "No error, no timeout, no way to tell which half of preflight is stuck"
  - "Quitting the app is the only way out, which destroys the evidence"
root_cause: api_misuse
resolution_type: code_fix
severity: medium
related_components:
  - RcGitKit
tags: [foundation, process, filehandle, readabilityhandler, readdatatoendoffile, self-update, preflight, timeout]
---

# Rebuild and Relaunch spins forever at preflight: an unbounded out-of-process probe behind a one-line log

## Problem

`UpdateModel.preflight` does two things out of process — probe the login shell for `cargo`,
`swift`, and `cbindgen`, then read the checkout through the core — and logged one line before
both of them. Neither had a time limit. A probe that never answered left the window spinning
with a single status line, no failure, and no way to tell which half it was sitting in.

## Symptoms

- The window shows "Checking the toolchain and the checkout…", spinner running, button
  disabled, for as long as it is left open.
- The log holds exactly `checkout: <root>` — the line written before the toolchain probe.
- `Rebuild and Relaunch` is the dev edit-run loop, so the loop is dead until the app is quit
  and `scripts/build-app.sh --dev` is run from a terminal instead.

## What Didn't Work

The obvious suspects were measured and neither explains a hang:

- The shell probe: `/bin/zsh -lc 'for tool in cargo swift cbindgen; do command -v $tool
  >/dev/null || echo $tool; done'` ran 30 times through a Swift copy of `Shell.run` at
  50–90 ms each, and again under `env -i` to mimic a Dock launch's empty environment. No user
  or system zsh startup file spawns a background process that could hold the pipe open.
- The checkout read: `cargo run -p rc-git-cli -- --time changes .` reports
  `open 8.34 ms, changes 216.62 ms` on this repository, with the 16 GB `target/` pruned as
  ignored. The app uses the same `worktree_status` at the same 200,000 scan cap.

So the stall is something about the app's own process that a terminal does not reproduce, and
the app had already been quit by the time anyone looked — there was no stuck process left to
`sample`. **The cause is still unproven.** What was fixed is everything that made it
undiagnosable, plus one genuine hazard found while reading the runner.

## Solution

Three changes, in `mac/Sources/RcGitKit/UpdateModel.swift` and
`mac/Sources/RcGitApp/UpdateEnvironmentLive.swift`.

**One reader per pipe.** The old runner set a `readabilityHandler` *and* then called
`readDataToEndOfFile()` on the same descriptor — two readers, the second of which blocks until
EOF:

```swift
// before
pipe.fileHandleForReading.readabilityHandler = { handle in collector.take(handle.availableData) }
await exit.wait()
pipe.fileHandleForReading.readabilityHandler = nil
collector.take(pipe.fileHandleForReading.readDataToEndOfFile())   // waits for EOF, forever
```

```swift
// after: one thread owns the descriptor and ends at EOF
private static func drain(_ handle: FileHandle, into collector: LineCollector) -> Signal {
    let finished = Signal()
    Thread.detachNewThread {
        while true {
            let data = handle.availableData
            if data.isEmpty { break }
            collector.take(data)
        }
        finished.signal()
    }
    return finished
}
```

**A limit on the probe.** `Shell.run` takes an optional `timeout`; a watchdog `Task` sends
`SIGTERM` at the limit and `SIGKILL` two seconds later, and the outcome carries `timedOut`.
The post-exit drain is bounded the same way (`await drained.wait(upTo: drainGrace)`), so a
grandchild holding the write end costs two seconds rather than the session.

**A probe that can report its own failure.** `UpdateEnvironment.missingTools() -> [String]`
became `toolchain() -> ToolchainCheck`, which is `.ready`, `.missing([String])`, or
`.failure(String)`. A 20-second timeout, a non-zero shell status, or a shell that cannot run
now all produce a preflight failure naming what to try in a terminal, instead of an empty
list that reads as "every tool is present".

**A log that localizes.** Each half is noted *before* it runs — `checking the toolchain`, then
`reading the checkout` — so the last line in the log is the step that stalled.

## Why This Works

`readDataToEndOfFile()` returns when the pipe reaches EOF, and a pipe reaches EOF when every
write end is closed. Foundation closes the parent's copy after the spawn, so normally the
child's exit is enough — but any grandchild that inherited the descriptor keeps it open after
the child is reaped, and `terminationHandler` has already fired by then. The await on the exit
signal returns and the process is gone, yet the read never finishes: a hang with a completed
process and no error, which is exactly the shape observed. Draining on one thread that stops at
EOF, and bounding the wait for that thread, removes the shape whether or not it was this
session's cause.

The rest is diagnosis, not repair. An out-of-process step that cannot time out has only two
outcomes, success and forever; adding the third makes the failure land in the window where the
user is already looking.

## Prevention

- **Every out-of-process step in the updater is bounded.** A probe gets a hard limit and a
  kill; a build does not (it legitimately takes minutes) but streams its output, so the log
  keeps moving. Nothing may have only "success" and "forever" as outcomes.
- **Log a step before running it, not after.** The log is the only account of where an update
  is sitting. `preflightLogsBothHalvesBeforeRunningThem` in
  `mac/Tests/RcGitKitTests/UpdateModelTests.swift` pins the order.
- **A probe reports why it could not answer.** A protocol method returning a bare collection
  ("which tools are missing") cannot say "the probe itself failed", and an empty result then
  reads as success. Give it a result type with a failure case.
- **Never set a `readabilityHandler` and call `readDataToEndOfFile()` on the same
  `FileHandle`.** Pick one reader. The streaming handler plus a bounded EOF signal covers both
  needs.
- **Sample the process while it is stuck.** `sample RCGit 5 -f /tmp/rcgit.txt` gives the stack
  that a measurement from a terminal cannot; quitting the app first throws that away. This one
  is still open for want of that sample.

## Related

- [A self-update relaunch loses the open tabs](userdefaults-handover-race-self-update-relaunch.md)
  — the other cross-process hazard in the same update path, on the handover side.
