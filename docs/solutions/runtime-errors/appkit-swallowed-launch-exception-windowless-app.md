---
title: "AppKit swallows a launch exception: app runs with no window"
date: 2026-09-07
category: runtime-errors
module: RcGitApp
problem_type: runtime_error
component: frontend
symptoms:
  - "App process launches and stays alive but no window ever appears; Dock icon shows, zero windows ordered on screen"
  - "Unified log and stderr are completely silent; no crash report is written"
  - "CGWindowList shows the app's windows with valid frames but kCGWindowIsOnscreen false"
  - "Process sample shows SOME_OTHER_THREAD_SWALLOWED_AT_LEAST_ONE_EXCEPTION (HIServices) with main thread idle in the run loop"
  - "Only reproduces when a saved NSWindow frame autosave differs in size from the initial contentRect (first-ever run works)"
root_cause: logic_error
resolution_type: code_fix
severity: critical
framework_version: macos 26.6.2
related_components: [appkit, window-lifecycle]
tags: [appkit, nswindow, frame-autosave, swallowed-exception, windowless-launch, windowdidresize, init-order, nsapplicationcrashonexceptions]
---

# AppKit swallows a launch exception: app runs with no window

## Problem

`RCGit.app` launched into a running, windowless process: the Dock icon appeared and the app checked in as a Foreground app, but no window was ever shown. Every launch after the first failed this way; the first-ever launch worked.

## Symptoms

- `open RCGit.app` succeeds; `pgrep -x RCGit` shows the process alive; `lsappinfo info` reports `type="Foreground"` with a normal window-server checkin.
- Zero visible windows. `CGWindowListCopyWindowInfo([.optionAll], ...)` lists the app's windows with valid frames, `alpha=1`, `layer=0` — but `kCGWindowIsOnscreen=false` on all of them, while other apps' windows report onscreen normally.
- Unified log for the process is completely empty (`log show --predicate 'process == "RCGit"'` returns nothing).
- No crash report in `~/Library/Logs/DiagnosticReports/`.

## What Didn't Work

- **Static launch checks — all clean, all red herrings.** Bundle structure, `codesign --verify`, no quarantine xattr, `spctl` (ad-hoc "rejected" is normal and does not block local launches), exec bit, arch, `LSMinimumSystemVersion`, single LaunchServices registration. Direct exec of the binary stayed alive with empty stderr.
- **First fix attempt: re-centering an off-screen restored frame.** Defaults held `NSWindow Frame rc-git.window = -1080 -40 1080 1355` — saved on an external display that sat left of the main screen and was later detached. A guard was added to re-center when the restored frame intersects no attached screen. Real defect, but secondary: the window still never appeared, because the code never got that far.

## Solution

Fix in `mac/Sources/RcGitApp/MainWindowController.swift`, in `init()`.

Before — delegate wired, then frame restored, views built last:

```swift
super.init(window: window)
window.delegate = self
window.center()
window.setFrameAutosaveName("rc-git.window")   // resize fires windowDidResize → applyLayout → crash
buildSplitViews()
buildToolbar()
wireModel()
applyLayout()
```

After (`MainWindowController.init()`, as it reads today) — views exist before anything can resize
the window. The restore has since moved from `setFrameAutosaveName` to
`setFrameUsingName(Self.frameName)`, because AppKit grants an autosave name to one live window
only and every tab is its own window; it resizes the window the same way:

```swift
super.init(window: window)
window.delegate = self
// Views must exist before the frame restore below: restoring resizes the window,
// which fires windowDidResize and runs applyLayout.
buildSplitViews()
buildToolbar()
buildVersionBadge()
wireModel()
// ...wireRefs(), wireHistory(), wireTree(), wireFile(), renderBanner()
window.center()
_ = window.setFrameUsingName(Self.frameName)
// A frame saved on a display that is now detached restores fully off-screen.
if !NSScreen.screens.contains(where: { $0.visibleFrame.intersects(window.frame) }) {
    window.center()
}
applyLayout()
```

And `applyLayout()` no longer indexes a possibly-empty array (`MainWindowController.applyLayout()`):

```swift
// before:  let sidebar = outerSplit.splitViewItems[0]   (after the size guard)
guard let size = window?.contentView?.frame.size,
    let sidebar = outerSplit.splitViewItems.first
else { return }
```

Verified with the poison frame still saved in defaults and `NSApplicationCrashOnExceptions` armed: no crash, main window `onscreen=1`. Fix is in the repo's initial commit.

## Why This Works

The failure chain:

1. `window.delegate = self` was set before `buildSplitViews()` ran.
2. `setFrameAutosaveName("rc-git.window")` restores the saved frame during `init`. When the saved size differs from the initial `contentRect`, the restore resizes the window.
3. The resize synchronously posts `NSWindowDidResizeNotification` → the delegate's `windowDidResize` → `applyLayout()`.
4. `applyLayout()` ran `outerSplit.splitViewItems[0]` — but `buildSplitViews()` had not run yet, so the array was empty → `NSRangeException`.
5. The exception unwound into AppKit's launch Apple Event processing (`aeProcessAppleEvent` → `_NSAppleEventManagerGenericHandler`), where **AppKit swallows it silently** — nothing on stderr, nothing in the unified log. `applicationDidFinishLaunching` aborted before reaching `showWindow(nil)`, so the windows existed in the window server (created with `defer: false`) but were never ordered in. The event loop then ran normally: a healthy-looking, windowless app.

Reordering `init` makes the split view items exist before any resize can fire `applyLayout`; the `.first` guard makes the callback safe even if a future change reintroduces an early resize. This also explains the reproduction pattern: with no saved frame (first run) the restore does not resize, so nothing fires — every run after a frame was saved crashed.

## Prevention

**Diagnosis recipe for a "process runs, no window, logs silent" macOS app:**

1. `sample <pid> 2` — the smoking gun was a thread stack frame reading `SOME_OTHER_THREAD_SWALLOWED_AT_LEAST_ONE_EXCEPTION (in HIServices)` while the main thread sat idle in the run loop.
2. `defaults write <bundle-id> NSApplicationCrashOnExceptions -bool true`, relaunch. The swallowed exception becomes a real crash; the `.ips` report's `asiBacktraces` holds the original throw site with symbols (here: `applyLayout` ← `windowDidResize` ← `_setFrameCommon` ← `_setFrameAutosaveName:` ← `MainWindowController.init` ← `applicationDidFinishLaunching`).
3. Delete the default after diagnosis.
4. `CGWindowListCopyWindowInfo` with `.optionAll` distinguishes "window never created" from "created but never ordered in" (`kCGWindowIsOnscreen`) from "ordered in off-screen" (bounds).

**Code rules:**

- Never set a window/view delegate before the state its callbacks touch exists. `windowDidResize` can fire inside `init`.
- Treat `setFrameAutosaveName(_:)` as an immediate `windowDidResize` source, not passive bookkeeping — it restores (and resizes) at the call site.
- In delegate callbacks that can fire during construction, prefer `guard let x = collection.first` over `collection[0]`.
- An exception thrown during launch Apple Event handling is swallowed by AppKit: the symptom is a windowless-but-running app with empty logs, not a crash. Reach for `NSApplicationCrashOnExceptions` early when launch behavior is inexplicable.

## Related Issues

- `.claude/rules/swift.md` states the construction-order rule for `MainWindowController.init`
  and links here.
- [Zero-height panes from a split view's autosave](../ui-bugs/nsstackview-zero-height-collapse-splitview-autosave.md)
  — another restored-state bug where `defaults read` separates stored state from live layout.
- [Tabs lost across a self-update](userdefaults-handover-race-self-update-relaunch.md) — the
  same launch path, from the side of the state the new process reads.
