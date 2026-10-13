---
title: "A self-update relaunch loses the open tabs: UserDefaults is not flushed before the new process reads it"
date: 2026-09-20
category: runtime-errors
module: RcGitApp
problem_type: runtime_error
component: frontend
symptoms:
  - "Rebuild and Relaunch brings the app back with one empty window; the repository tabs that were open are gone"
  - "`defaults read com.timeturnback.rcgit.dev` still lists the tabs under `rc-git.openTabs` while the relaunched app shows none"
  - "A manual quit and launch restores the same tabs correctly; only the self-update path drops them"
  - "No error is logged and the update window reports success"
root_cause: async_timing
resolution_type: code_fix
severity: medium
framework_version: macos 26.6.2
related_components: [userdefaults, cfprefsd, app-lifecycle, self-update]
tags: [userdefaults, cfprefsd, synchronize, nsworkspace, openapplication, relaunch, self-update, cross-process, race-condition, state-handover]
---

# A self-update relaunch loses the open tabs: UserDefaults is not flushed before the new process reads it

## Problem

`Rebuild and Relaunch` (dev bundle) and `Update rc-git` (release bundle) save the open tabs,
launch the new build, and terminate. The relaunched build came up with one empty window even
though the tab list was in `UserDefaults` — because it read the list before the old process's
write reached disk.

## Symptoms

- The relaunched app shows a single empty window; the repository tabs are gone.
- `defaults read com.timeturnback.rcgit.dev` shows `rc-git.openTabs` still holding the
  repository path. The value is intact; the new process simply did not see it.
- Quitting and launching by hand restores tabs correctly. Only the self-update path loses them.
- Nothing is logged. The update window reports success.

## What Didn't Work

The obvious suspect was the change shipped in the same session: `scripts/build-app.sh` had just
started stamping `CFBundleShortVersionString` with the commit count, so the bundle's version
string differed between the old and new build.

That was ruled out by reading the domain rather than the code. `UserDefaults` is scoped by
`CFBundleIdentifier`, which `scripts/build-app.sh` stamps per variant and which no version
change touches — and the dump proved the point directly: the tab list was *present* in the
domain while the running app showed no tabs. A persistence bug cannot leave the value on disk.
The failure was on the read side, not the write side.

## Solution

Flush the write before the replacement process launches, and stop writing once the state has
been handed over.

`mac/Sources/RcGitKit/OpenTabs.swift:34`:

```swift
public func handOff(paths: [String], selectedIndex: Int) {
    save(paths: paths, selectedIndex: selectedIndex)
    defaults.synchronize()
}
```

`mac/Sources/RcGitApp/AppDelegate.swift:150`:

```swift
func saveTabs() {
    guard !tabsHandedOff else { return }
    let state = tabState()
    openTabs.save(paths: state.paths, selectedIndex: state.selectedIndex)
}

func handOffTabs() {
    let state = tabState()
    openTabs.handOff(paths: state.paths, selectedIndex: state.selectedIndex)
    tabsHandedOff = true
}
```

`UpdateWindowController`'s `willRelaunch` closure now calls `handOffTabs()` instead of
`saveTabs()`. Fixed in commit `ea73c00` on `master`; at the time of writing the round trip has
not been confirmed by a user-run rebuild, and the first rebuild after the fix still runs the
old binary's handover.

## Why This Works

`UpdateEnvironmentLive.relaunch()` (`mac/Sources/RcGitApp/UpdateEnvironmentLive.swift:218`)
runs in this order:

1. `willRelaunch()` — save the tabs.
2. `NSWorkspace.shared.openApplication` with `createsNewApplicationInstance = true`.
3. `NSApp.terminate(nil)`.

Between 2 and 3 **two processes of the same bundle identifier are alive at once**, and the new
one reads `rc-git.openTabs` during `applicationDidFinishLaunching`, milliseconds after step 1.
A `UserDefaults` write is handed to `cfprefsd` asynchronously and committed to the domain's
plist lazily; a process that starts fresh reads what was last committed. The new process
therefore read a pre-save snapshot of the domain, found no tabs, and opened an empty window.
`synchronize()` forces the commit before the launch, which is exactly the cross-process
handover case the method still exists for.

Why the value was nonetheless on disk afterwards: at the time, `restoreTabs()` returned
without saving when it restored nothing, so the new process never clobbered the list; the old
process then wrote it again from `applicationWillTerminate`. The symptom was a lost session,
never a lost list — which is why the next manual launch looked fine and hid the bug.

`restoreTabs()` (`mac/Sources/RcGitApp/AppDelegate.swift`) now always saves the list that
`OpenTabs.restoredPaths` decides: the tabs that opened, then each repository that failed to
open while its parent folder was unreachable (a volume not mounted yet). It drops only a
repository gone from a folder that exists, logs every failure under the `tabs` category, and
never clears the list because nothing opened.

`tabsHandedOff` closes the mirror image of the same window: the old process outlives the new
one's launch, and its termination write would otherwise land on top of whatever the new process
had already recorded.

## Prevention

- **State handed to another process needs an explicit flush.** Any `UserDefaults` value a
  second process reads while this one is still alive — a relaunch, a helper, an XPC service —
  must be `synchronize()`d at the handover. `synchronize()` is discouraged for ordinary saves
  and correct here; that is the distinction, not "never call it".
- **A handover is one-way.** After handing state over, stop writing it. Two live processes
  writing one defaults domain is last-writer-wins, and the departing one usually writes last.
- **Read the domain before reading the code.** `defaults read <bundle-id>` separates a write
  bug from a read bug in one command. A value present on disk while the UI disagrees means the
  reader raced, not that persistence broke.
- Related: the bundle identifier is what scopes this state, which is why it must never depend
  on a version or build number (`CLAUDE.md`, and
  `docs/solutions/runtime-errors/appkit-swallowed-launch-exception-windowless-app.md` for the
  other launch-time failure in this app that also left no log trail).
