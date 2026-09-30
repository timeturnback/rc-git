---
title: "Back/forward stops are recorded only from a real change, never re-asserted"
date: 2026-09-30
category: architecture-patterns
module: RcGitKit
problem_type: architecture_pattern
component: frontend
severity: medium
related_components:
  - RcGitApp
applies_when:
  - "Adding a new way to move in the app (a jump, a link, a picker, a pane switch) that Back should return from"
  - "A restore can miss its stop because the file was deleted or the ref is gone"
  - "Tempted to record the current location from a lifecycle event or every model callback, to be safe"
  - "Back or Forward can be pressed again while the previous restore is still loading"
  - "Building any history whose current entry can disagree with what is on screen"
tags:
  - back-forward-list
  - navigation-history
  - navigation-stop
  - browser-model
  - failed-restore
  - forward-stops
  - coalescing
  - generation-counter
---

# Back/forward stops are recorded only from a real change, never re-asserted

## Context

The back/forward feature keeps one `BackForwardList` for the whole app, owned by `AppDelegate`
(`mac/Sources/RcGitApp/AppDelegate.swift:14-16`). A stop is a tab's repository path, its pane,
and a `BrowserLocation`. The location names a tree, a file path, a history scope and a detail
commit, and holds none of them (`mac/Sources/RcGitKit/BackForwardList.swift:21-53`).
`BrowserModel.restore` reads what the stop names from the repository as it is at that moment
(`mac/Sources/RcGitKit/BrowserModel.swift:920-946`).

So a restore can end somewhere other than its stop. A deleted ref reports an error and leaves
the browser where it was (`BrowserModel.swift:989-992`). A file that is no longer in the tree
does not open (`BrowserModel.swift:479-481`), and the restore empties the pane, so the file of
the stop left behind does not stay open under another stop's name
(`BrowserModel.swift:953-963`). From then on the list's current stop and the screen disagree
until the user moves.

`BackForwardList.visit` ignores only a stop equal to the current one. Any other stop drops the
stops ahead and is appended (`BackForwardList.swift:107-117`). While list and screen disagree,
a call that records "where the tab is" counts as a move to a new place, and Forward is gone
with the user having done nothing.

## Guidance

Record a stop only when something changed. Do not record to re-assert where the tab already
is. The tree has three sources of a record:

- **The model's location changed.** `noteLocation` compares `location` with `reportedLocation`
  and fires `onLocationChanged` only on a difference, and only while no move is under way
  (`BrowserModel.swift:1025-1033`). A method whose change spans an await brackets itself in
  `beginMove`/`endMove`, so it reports once, where it ended (`BrowserModel.swift:1016-1023`).
  The window controller turns the callback into `recordStop()`
  (`mac/Sources/RcGitApp/MainWindowController.swift:674-676`).
- **The user acted on the pane.** `switchMode` is `setMode` plus `recordStop()`, for a pane
  picked by itself (`MainWindowController.swift:203-208`). An action that changes the pane on
  its way somewhere calls `setMode`, then the model, then `recordStop()` when the call returns
  (`MainWindowController.swift:391-397`, `447-453`, `729-736`, `741-748`, `811-821`). When the
  model already reported the move, that last record equals the current stop and `visit` drops
  it. It covers the case where the pane changed and the location did not.
- **Another tab came to the front.** `cameToFront` returns when the controller is already
  `frontController`, which is what the same window becoming main on app reactivation looks
  like (`AppDelegate.swift:219-227`). `go` sets `frontController` before it shows the stop's
  window, so the tab switch a step makes is not recorded either (`AppDelegate.swift:199-200`).

`AppDelegate.record` then filters what arrives: nothing while `restoresInFlight` is non-zero,
and nothing from a tab that is not in front (`AppDelegate.swift:208-217`). A restore that moved
the browser still ends with a model report, because `endMove` runs in `restore`'s `defer`
(`BrowserModel.swift:938-941`). The app drops that report, and `reportedLocation` now matches
the screen, so a later ref refresh that moves nothing has no difference to report.

## Why This Matters

Four other designs were considered while building the feature and rejected.

1. **Back returns to the list's current stop when the screen is not on it.** This would fix the
   empty-tab gap below. A stop that cannot be restored never matches the screen, though: the
   deleted-ref restore changes nothing (`BrowserModel.swift:989-992`). Each Back press would
   retry the same stop and the user could not get past it.
2. **Rewrite the current stop with where the restore ended.** The rewritten stop can equal its
   neighbor, and then one Back press changes nothing. `removeStops` merges equal neighbors for
   the same reason (`BackForwardList.swift:133-135`). The list has no call that rewrites a
   stop; `stops` is `private(set)` (`BackForwardList.swift:88`).
3. **Record from every model callback.** `select` sets `selectedRef` before its await, and
   `loadRoot` closes the open file after it (`BrowserModel.swift:278-283`, `411-413`). A record
   in between is the new ref with the old file, a stop nobody was at.
4. **Let restores overlap and drop the stale one with a generation check after each await.**
   `location.tree` comes from `selectedRef` (`BrowserModel.swift:904-914`), which `select` sets
   synchronously. A second restore into the same tree would find the tree already matching,
   skip the tree step of `move(to:)` (`BrowserModel.swift:949-952`), and resolve its file path
   against the `rootTree` the first restore has not replaced yet (`BrowserModel.swift:479`).
   Restores coalesce instead, with `isRestoring` and `pendingRestore`
   (`BrowserModel.swift:932-946`), the shape `ChangesModel.loadStatus` uses for scans
   (`mac/Sources/RcGitKit/ChangesModel.swift:86-95`).

Accepted gap: a tab with no repository has no stop, since `currentStop` is `nil`
(`MainWindowController.swift:224-230`). Coming to the front records nothing, and the list's mark
stays on the tab the user left. Back pressed in the empty tab goes to the stop before that one.

A second gap, read from the source and not covered by a test: `refreshRefs` brackets itself in
`beginMove`/`endMove` as well (`BrowserModel.swift:214-215`). When a watcher-driven refresh is
still under way as a restore ends, the restore's `endMove` reports nothing and
`restoresInFlight` returns to zero. The refresh's own `endMove` then reports where the restore
ended, and `AppDelegate.record` accepts it. After a restore that fell short, that report is a
stop that differs from the current one, so it drops the stops ahead.

## When to Apply

- Adding a `BrowserModel` method that changes the tree, the open file, the history scope or the
  detail commit. Call `noteLocation()` after the change, or bracket the method in
  `beginMove()`/`endMove()` when the change spans an await. Do not fire `onLocationChanged`
  directly.
- Adding a UI action that changes the pane. Use `setMode`, the model call, then `recordStop()`.
  Use `switchMode` only when the pane is the whole action.
- Wanting to call `recordStop()` or `visit` from a lifecycle event to keep the list in step
  with the screen: the window became main, refs refreshed, a restore finished, a repository
  attached. The rule excludes those calls.
- Reviewing a change to `AppDelegate` or `MainWindowController` near the list. `RcGitApp` has
  no tests, so `record`, `cameToFront` and `restoresInFlight` are checked by reading them
  against the three sources above.
- Building any history whose entries name things and re-read them on return.

## Examples

The model reports a difference and nothing else (`BrowserModel.swift:1027-1033`):

```swift
private func noteLocation() {
    guard moveDepth == 0 else { return }
    let current = location
    guard current != reportedLocation else { return }
    reportedLocation = current
    onLocationChanged?()
}
```

The same window becoming main again is not a move (`AppDelegate.swift:221-227`):

```swift
private func cameToFront(_ controller: MainWindowController?) {
    guard let controller, controller !== frontController else { return }
    frontController = controller
    if let stop = controller.currentStop {
        record(stop, from: controller)
    }
}
```

What the `frontController` guard prevents: the list is `[A, B, C]` with the mark on `B` after
one step back, and `B`'s file is gone, so the screen shows `B`'s ref with no file. The user
switches to another app and back. A `windowDidBecomeMain` that recorded `currentStop` would
visit a stop that differs from `B`, and `visit` would drop `C`.

Tests that pin the behavior, in `mac/Tests/RcGitKitTests/`:

- `everyMoveReportsOnce` (`BrowserLocationTests.swift:89`): the same file opened again reports
  nothing, and neither does "a refresh that moved nothing".
- `aRefSwitchReportsWhereItEndedAndNotTheStepBetween` (`BrowserLocationTests.swift:109`): one
  report for a ref switch, without the old file.
- `restoreReportsWhereItEndedOnce` (`BrowserLocationTests.swift:245`) and
  `twoQuickRestoresIntoAnotherTreeEndOnTheSecond` (`BrowserLocationTests.swift:284`): one
  report for a restore and one for a coalesced pair, and the second of two overlapping
  restores opens its file against the tree the first one loaded.
- `restoreOfAFileGoneFromTheSameTreeClosesTheOneOpen` (`BrowserLocationTests.swift:227`): a
  restore that falls short leaves an empty pane, which is the state the list then disagrees
  with.
- `visitingTheCurrentStopRecordsNothing` (`BackForwardListTests.swift:39`) and
  `aVisitAfterSteppingBackDropsTheStopsAhead` (`BackForwardListTests.swift:61`): the two halves
  of `visit` that make a record without a change cost the stops ahead.

## Related

- [A Task per filesystem event](../runtime-errors/task-per-watch-event-backlog.md): where the
  in-flight flag plus one pending slot comes from. There it bounds work a watcher asks for; here
  it orders two restores, and the pending slot carries the newest location.
- `.claude/rules/swift.md` states the rules for `noteLocation`, `setMode` then `recordStop`, and
  the one list in `AppDelegate`. `docs/architecture.md` ("Going back") has the flow.
- `CONCEPTS.md` defines a stop.
