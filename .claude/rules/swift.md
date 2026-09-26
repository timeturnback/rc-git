---
description: how the Swift package is split and the AppKit patterns the app relies on
paths: mac/**
---

# Swift

- Logic lives in `RcGitKit`, which never imports AppKit and is what `swift test` reaches. Views
  in `RcGitApp` forward events to `BrowserModel` and render its callbacks; they hold no state
  the model does not have.
- Every asynchronous load bumps a generation counter and checks it before applying the result.
  A user who clicked something else must never see the older result land.
- Repository calls run inside `Task.detached`; the model is `@MainActor` and callbacks fire on
  the main actor. `Repository` is `@unchecked Sendable` because the Rust side is thread-safe.
- Window size decisions go through `LayoutPolicy.layout(for:)` and its tests. The window
  controller applies the result and remembers whether it collapsed the sidebar itself, so a
  user's manual collapse is respected.
- The sidebar is the navigation for every mode: `SidebarViewController` keeps the refs on top
  and swaps the mode's section under them (tree, or the Changes list and commit box). A
  section is hidden through `detachesHiddenViews`, and AppKit drops an ancestor's constraints
  for a detached view for good, so a section carries no constraint to anything outside itself
  — size it with hugging and with a toggled low-priority height on the refs, which never leave.
- A view the stack has never laid out while attached gets no alignment constraint either, and
  unhiding does not backfill one: anything added hidden (a section, the truncation note) sizes
  to its content until `show` re-makes its width constraint by hand
  (`docs/solutions/ui-bugs/nsstackview-hidden-at-attach-never-gets-width-alignment.md`).
- The sidebar collapses with `.preferResizingSiblingsWithFixedSplitView`, so the window keeps
  its size; `.preferResizingSplitViewWithFixedSiblings` shrinks the window instead. The
  toolbar carries no `.sidebarTrackingSeparator`: `outerSplit` sits inside
  `DropHostViewController` rather than being the window's content controller, and a separator
  that cannot measure its split throws the items after it across the titlebar on collapse.
- A cell whose label carries an `NSAttributedString` puts its truncation in the string's
  `.paragraphStyle`; `lineBreakMode` on the field only applies to a plain `stringValue` and the
  text clips with no ellipsis. Keep a row's controls in one `NSTableCellView` on `centerYAnchor`
  rather than in separate table columns
  (`docs/solutions/ui-bugs/attributed-string-label-ignores-cell-linebreakmode.md`).
- The Changes list is one `NSOutlineView` serving both groupings from `ChangeTree` (RcGitKit,
  pure and tested); flat rows are childless roots. Node identity dies on every reload, so
  expansion and selection are restored by path, never by row index.
- The file pane uses a TextKit 1 stack with `allowsNonContiguousLayout` and no wrapping; the
  line-number ruler assumes one fragment per line. Do not turn wrapping on without changing
  `LineNumberRulerView` — the markdown preview mode may wrap only because it hides the ruler.
  Syntax colors are temporary attributes applied per visible chunk; never add a layout-
  affecting attribute (font, paragraph style) through that path.
- In `MainWindowController.init`, views are built before `setFrameAutosaveName`: the frame
  restore resizes the window and fires `windowDidResize` -> `applyLayout()` synchronously, and an
  exception there is swallowed by launch Apple Event handling, leaving the app windowless
  (`docs/solutions/runtime-errors/appkit-swallowed-launch-exception-windowless-app.md`). Keep
  delegate callbacks safe during construction: `.first`, not `[0]`.
- Icons come from `Icons` (cached per extension). Do not call `NSWorkspace` from cell code.
- Anything that differs between the installed app and the dev bundle reads `AppBuild`
  (variant, source checkout, commit, `updateMode`, `relaunchPath`); never test the bundle
  identifier by hand. Work the updater does outside the process — toolchain probe, checkout
  read, pull, build, relaunch — goes behind `UpdateEnvironment` so `UpdateModel` stays
  testable.
- Every step the updater runs outside the process is bounded and is logged *before* it runs,
  never after: the probe carries a timeout and a kill, the result type has a failure case that
  says it could not answer, and the log's last line is the step that stalled. Nothing may have
  only "success" and "forever" as outcomes
  (`docs/solutions/runtime-errors/unbounded-preflight-probe-spins-with-no-step-log.md`).
- One reader per `Pipe`. Setting a `readabilityHandler` and also calling
  `readDataToEndOfFile()` reads one descriptor from two places, and the second blocks until
  EOF — which a grandchild that inherited the write end never delivers, long after
  `terminationHandler` has fired. Drain on a thread that stops at EOF, and bound that wait too.
- Closing a tab is not a decision about the next launch. `OpenTabs.record` merges the live
  tabs into the saved list and never drops one, so ⌘W, ⇧⌘W and ⌘Q all relaunch with the same
  tabs; `OpenTabs.save` replaces the list and only `restoreTabs` calls it, to drop a
  repository that no longer opens.
- State the relaunched build reads — the open tabs — is handed over, not just saved:
  `OpenTabs.handOff` flushes the write because both processes are alive at the launch, and
  `AppDelegate.tabsHandedOff` stops the old process writing afterwards
  (`docs/solutions/runtime-errors/userdefaults-handover-race-self-update-relaunch.md`).
- `UpdateModel` runs one of two modes. `.update` (release) fetches and refuses a dirty
  checkout; `.rebuild` (dev) builds the working tree as it stands and has no up-to-date
  shortcut, because the tree moves without the commit. Do not make `.rebuild` fetch.
- Tests use `FakeRepository` for behavior and `GitFixture` for the real bridge; both build the
  same standard repository as `rc-git-testkit`, keep them in step.
- `xcrun swift-format lint --strict` must pass; `.swift-format` at the root is the config.
- Never collapse a view with a height-0 constraint while it has visible subviews — hide it
  inside an `NSStackView` with `detachesHiddenViews`.
- A bare `NSSplitView` sizes its panes by frame, so an Auto Layout floor on a pane only fights
  the frame it is handed and loses silently. Own `splitView(_:resizeSubviewsWithOldSize:)` and
  place both panes through `LayoutPolicy.splitPane`, put the minimum on the split itself (both
  floors plus the divider), and re-clamp in `viewDidLayout` — that is where a restored divider
  position first reaches the screen, and a clamp that keeps a legal position converges in one
  pass. `constrainMinCoordinate`/`constrainMaxCoordinate` bound a *drag* only: not
  `adjustSubviews`, not the autosave restore, which is why `.v4` was poisoned like `.v3`.
- Version `autosaveName` to discard poison already stored, never as the fix — a bump alone just
  resets the clock. `NSSplitViewController` items are exempt: their `minimumThickness` already
  does this (`MainWindowController` sets it on both items of `outerSplit`).
- Diagnose a pane stuck at zero with
  `defaults read <bundle-id> "NSSplitView Subview Frames <autosaveName>"` before touching layout
  code — entries are `x, y, w, h, isCollapsed, isHidden`, and it separates a live layout bug from
  restored state without launching the app
  (`docs/solutions/ui-bugs/nsstackview-zero-height-collapse-splitview-autosave.md`).
- A model whose load is driven by a watcher coalesces; it never queues. `ChangesModel` keeps a
  `scanning`/`pending` pair per load: a request made while one runs sets `pending` and returns,
  and the running loop re-reads once when it lands. Both flags are only touched between
  suspension points on the main actor, so no request is lost. Spawning a `Task` per FSEvents
  callback built a backlog of 190 000 suspended tasks and 638 MB of task stacks, because the
  generation counter drops a stale *result* and never stops the work that produces it
  (`docs/solutions/runtime-errors/task-per-watch-event-backlog.md`).
- `cancelScans()` is the most a cancel can do here: the repository call is synchronous FFI and
  runs to completion. It drops the queued re-run, bumps the generation so the in-flight result
  never lands, and resets `.loading` to `.idle` so the next switch back to the tab reloads
  rather than showing a spinner nothing will ever fill.
