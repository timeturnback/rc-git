---
title: "A Task per filesystem event: 190 000 suspended tasks, 638 MB, and ten pinned cores"
date: 2026-09-26
last_updated: 2026-11-12
category: runtime-errors
module: rc-git-kit
problem_type: performance
component: frontend
symptoms:
  - "RCGit sits at 300–400% CPU for hours with no window in front and no user action"
  - "`top` reports 800 MB for the process, almost all of it compressed, and the number only ever grows"
  - "`footprint` puts 739 MB in MALLOC_SMALL — application heap, not graphics or caches"
  - "`sample` shows every cooperative-pool thread inside the same call: ChangesModel.loadStatus → worktreeStatus → StatusWalk::walk_dir"
  - "The Rust walk benchmarks clean in isolation: ten concurrent walks hold at 52 MB across repeated rounds"
  - "Quitting and reopening fixes it until the next long session with a busy working tree"
root_cause: logic_error
resolution_type: code_fix
severity: high
framework_version: macos 26.6.2, swift 6
related_components: [swift-concurrency, fsevents, worktree-status, gix-index]
tags:
  [
    swift-concurrency,
    task-backlog,
    coalescing,
    fsevents,
    debounce,
    generation-counter,
    worktree-status,
    index-stat-cache,
    memory-leak,
  ]
---

# A Task per filesystem event: 190 000 suspended tasks, 638 MB, and ten pinned cores

A dev build left open for eleven hours on two repositories held 803 MB and burned 329% CPU
with nothing on screen doing anything. The leak was not in the Rust core, which the process
spends all its time inside. It was one unstructured `Task` per filesystem event, and the
generation counter that looks like it guards against exactly this and does not.

The incident happened in the Swift app the GPUI app replaced, so the traces below name Swift
types. The rule it produced holds in the Rust port: `ChangesModel`
(`crates/rc-git-kit/src/changes/`) coalesces every watcher-driven load through a `Coalescer`.

## Symptom

```
$ top -l 2 -pid 84543 -stats pid,command,cpu,mem,cmprs
PID    COMMAND %CPU  MEM  CMPRS
84543  RCGit   329.4 803M 751M
```

`footprint` attributes 739 MB to `MALLOC_SMALL` — small heap allocations, not images, not
mmapped packs. `sample` finds all ten Swift cooperative-pool threads in one place:

```
closure #1 in ChangesModel.loadStatus()  ×10
  Repository.worktreeStatus(tree:maxScan:)   Repository.swift:375
    rc_git_core::worktree::StatusWalk::walk_dir
```

`heap` names the memory outright:

```
504859  565455872  1120.0   Task stack                        libswift_Concurrency.dylib
189367   60597440   320.0   Task stack                        libswift_Concurrency.dylib
189375    9090512     48.0   Closure context (unknown layout)  Swift
```

About 190 000 suspended tasks, 638 MB of task stacks. That count is the whole diagnosis: it is
not one runaway scan, it is eleven hours of arrivals that were never served and never dropped.

## Root cause

Two independent faults that only bite together.

**The model queued a task per event and cancelled none.** `WorktreeWatcher` fires,
`MainWindowController.worktreeDidChange()` spawns `Task { await changesModel.refresh() }`, and
`loadStatus()` bumps `statusGeneration` before starting a `Task.detached` walk. The generation
counter reads like a guard against pile-up. It is not: it discards a stale *result*, and does
nothing about the work producing it. The detached walk runs to completion regardless, and
`refresh()` starts a stash load beside it. Nothing bounds how many are outstanding.

**Each walk was an order of magnitude slower than it had to be.** `worktree::compare` hashed
every tracked file whose size matched its blob header. Git does not: it reads the index's
recorded `lstat` for each entry and skips the hash when it still matches disk. On a 13 185-file
checkout that was 1.8–2.8 s per walk against `git status`'s 0.10 s.

Put together they cross a threshold. FSEvents coalesces at a 0.5 s latency, so a working tree
under churn — a build, a dev server, a watch process — delivers roughly one callback a second.
A walk that takes longer than the interval between arrivals is a queue that never drains: the
pool runs ten at a time forever and every further request is added to a backlog whose only
bound is memory. Halve the arrival rate or halve the service time and the process looks fine
for hours; it is the crossing that turns a spike into eleven hours of accumulation.

The Rust side is not the leak. Ten concurrent walks in a standalone harness hold at 52 MB
across repeated rounds. The memory is entirely the Swift task backlog, which is why nothing in
the core's own budgets or tests caught it.

## Solution

**1. Coalesce in the model; never queue.** One scan in flight plus at most one queued re-run,
however many requests arrive. In the Rust port, `ChangesModel::load_status`
(`crates/rc-git-kit/src/changes/scan.rs`) opens with:

```rust
self.status.pending = true;
if self.status.running {
    return Task::ready(());
}
self.status.running = true;
```

and the running loop scans again while `pending` was set behind it. Both flags are only read
and written on the UI thread with no await between the loop's `pending` check and the
assignment that follows it, so a request that arrives during a scan always lands in the flag
the scan is about to read. Nothing is lost and nothing stacks.
The cost is that returning no longer means "your scan finished" — it means the model has
accounted for it. Say so in the doc comment; a caller that assumes otherwise is the next bug.

**2. Read the index as a stat cache.** `StatCache` in `worktree.rs` asks the index before
hashing: entry present, unconflicted, id equal to the tree's, no assume-valid / skip-worktree /
intent-to-add flag, not racy against the index's own timestamp, and a full `Stat::matches`
against a fresh `lstat`. Anything else falls through to the hash. It may only ever *decline* —
a cache that answers on partial evidence hides a real edit, which is far worse than a slow
scan.

**3. Cancel what can be cancelled, and say what cannot.** `cancel_scans` drops the queued
re-run and bumps the generation so the in-flight result never lands, but the repository call
is a blocking core call and runs to completion. It also resets a running scan's state to
`ChangesState::Idle`, because a cancelled load that leaves the state loading gives `set_mode`
no reason to reload and the user a spinner nothing will ever fill.

## Result

Same binary, same command, same repository, back to back:

| | before | after |
|---|---|---|
| `rc-git --time changes` (13 185 files) | 1795–2816 ms | 155–175 ms |
| scans per FSEvents burst | one per event | two |
| steady-state task backlog | unbounded | one in flight, one queued |

## Prevention

- A generation counter is not backpressure. It decides which result is allowed to land; it
  says nothing about how much work is running. Any load driven by a watcher needs an explicit
  in-flight flag as well.
- Before optimising a hot walk, check the *arrival* rate against the *service* time. A walk
  twice as fast is worth less than one that cannot be asked for twice at once, and a system
  where arrivals outpace service degrades without bound rather than gracefully.
- `heap <pid>` names the allocation by type. Reach for it before profiling the code you
  assume is guilty: "Task stack ×189 367" pointed at the scheduler in one line, after `sample`
  had spent the investigation pointing at the Rust walk.
- Benchmark the operation the app runs, not the one with the matching name. `rc-git status`
  lists a single directory (`worktree_entries`); the app's per-event call is the recursive
  `worktree_status`, which is `rc-git changes`. The first measured 7.9 ms and hid the problem
  completely.
