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
  `LineNumberRulerView`.
- Icons come from `Icons` (cached per extension). Do not call `NSWorkspace` from cell code.
- Tests use `FakeRepository` for behavior and `GitFixture` for the real bridge; both build the
  same standard repository as `rc-git-testkit`, keep them in step.
- `xcrun swift-format lint --strict` must pass; `.swift-format` at the root is the config.
