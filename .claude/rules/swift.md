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
- `UpdateModel` runs one of two modes. `.update` (release) fetches and refuses a dirty
  checkout; `.rebuild` (dev) builds the working tree as it stands and has no up-to-date
  shortcut, because the tree moves without the commit. Do not make `.rebuild` fetch.
- Tests use `FakeRepository` for behavior and `GitFixture` for the real bridge; both build the
  same standard repository as `rc-git-testkit`, keep them in step.
- `xcrun swift-format lint --strict` must pass; `.swift-format` at the root is the config.
- Never collapse a view with a height-0 constraint while it has visible subviews — hide it
  inside an `NSStackView` with `detachesHiddenViews`. Give scroll content in a split a
  min-height floor, and version `autosaveName` (`.v2`) whenever split/layout structure
  changes: autosave records broken layouts and re-imposes them on every launch
  (`docs/solutions/ui-bugs/nsstackview-zero-height-collapse-splitview-autosave.md`).
