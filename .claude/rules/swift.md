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
- The sidebar is the navigation for every mode: History shows the ref list alone; Files and
  Changes show `RefBannerView` over the mode's section (tree, or the Changes list and commit
  box). What the banner says comes from `BranchBanner` (RcGitKit) and nowhere else. Views are
  hidden through `detachesHiddenViews`, and AppKit drops an ancestor's constraints for a
  detached view for good, so a section carries no constraint to anything outside itself —
  size it with hugging.
- A view the stack has never laid out while attached gets no alignment constraint either, and
  unhiding does not backfill one: anything added or hidden before the first layout (a section,
  the ref list on a window that opens on Files, the truncation note) sizes to its content, so
  `show` re-makes the width of every view showing by hand
  (`docs/solutions/ui-bugs/nsstackview-hidden-at-attach-never-gets-width-alignment.md`).
- In Files the ref list lives in `RefPickerController`, a transient popover under the
  banner over the same `RefsViewController` in its `.picker` behavior: selection only
  highlights, a click or Return picks. Actions that ask with a sheet close the popover first.
  The banner opens it on mouse-down, and a transient popover closes on that same mouse-down
  when it is already open, so `chooseRef` ignores the event whose timestamp closed it.
  In Changes the banner is read-only: it names the checked-out branch through
  `BranchBanner.checkedOut`, because Changes always compares the working tree with HEAD, and
  ⌘B is disabled there (`canOpenRefPicker`).
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
  expansion and selection are restored by `ChangeNode.key`, never by row index or `path` (a
  file and a folder can share one path).
- The file pane uses a TextKit 1 stack with `allowsNonContiguousLayout` and no wrapping;
  `LineNumberRulerView` numbers only fragments that begin a line, so a wrapped continuation
  gets no number. Wrapping would leave gaps in the gutter, not shift it — the markdown preview
  mode wraps and hides the ruler.
  Syntax colors are temporary attributes applied per visible chunk; never add a layout-
  affecting attribute (font, paragraph style) through that path.
- Text positions are UTF-16 offsets into the decoded `String`. `TextDocument` maps them from the
  bytes when the core says the blob is valid UTF-8, and walks `text.utf16` only for lossy
  input: a U+FFFD replacement shifts every later offset, so bytes no longer map to it.
- The markdown preview is two halves: `MarkdownDocument.parse` (RcGitKit, pure, byte-level,
  tested) turns source into flat blocks and styled runs, and `MarkdownRenderer` (RcGitApp)
  decides how each block looks. A new syntax goes in the parser with a test; the renderer
  never re-reads the source. Both run off the main actor, like the plain paint.
- Tables are `NSTextTable` cells and blockquotes and code panels are `NSTextBlock`s carried in
  the paragraph style. Layout-affecting attributes are fine on that build path and never on
  the source-mode temporary-attribute path.
- An `NSTextBlock` that is not an `NSTextTableBlock` has no intrinsic width and lays out one
  glyph per line without one. Build every standalone block through `Theme.fullWidthBlock()`,
  which sets `setContentWidth(100, type: .percentageValueType)`
  (`docs/solutions/ui-bugs/nstextblock-without-content-width-collapses.md`).
- Check preview layout by measuring, not by launching: `NSTextStorage` + `NSLayoutManager` +
  a fixed-width `NSTextContainer`, then walk `lineFragmentRect`. A block fragment far narrower
  than the container is a collapsed block.
- Every markdown link resolves through `MarkdownLinkTarget.resolve` against the pane's
  `repositoryPath`. Do not read a destination anywhere else: telling a repository path, a URL
  and a `#fragment` apart in one place is what keeps clicking, the context menu and
  "Copy Link" from disagreeing. A repository link opens through `BrowserModel.openPath`, which
  walks one tree object per component. Its `#fragment` goes with it: the model keeps it as
  `pendingFragment` only once the load starts, and the pane takes it after it paints. Never
  hold a fragment in the view — a dead link would leave it for the next file to scroll to.
- Import links in source mode never enter the text storage: `LinkTextView` asks its owner
  which range a character falls in and draws the hover underline as a temporary attribute. A
  `.link` attribute would be a storage edit, which invalidates layout for every range it
  touches. Links load on the first pointer move (`FileViewController.importLink(at:)`), and a
  click resolves through `BrowserModel.openImport` and `ImportResolver` — never resolve a
  specifier in a view, and never keep a resolved path: the tree on screen decides.
- In `MainWindowController.init`, views are built before `setFrameUsingName`: the frame
  restore resizes the window and fires `windowDidResize` -> `applyLayout()` synchronously, and an
  exception there is swallowed by launch Apple Event handling, leaving the app windowless
  (`docs/solutions/runtime-errors/appkit-swallowed-launch-exception-windowless-app.md`). Keep
  delegate callbacks safe during construction: `.first`, not `[0]`.
- Every sidebar list goes through `Style.sidebarList` (a source list whose rows follow the
  system sidebar size) and every icon-and-label row is a `SidebarCellView`; anything above a
  list keeps `Style.margin` so it lines up with the selection.
- Never set a per-row font on a plain-string `textField` outlet: in any table whose
  `rowSizeStyle` is not `.custom`, AppKit re-applies a plain system font to that outlet at every
  layout. Use an attributed string, `.custom` row size, or a label that is not the outlet — as
  `SidebarCellView` does, whose tables answer type-select through their delegate
  (`docs/solutions/ui-bugs/nstablecellview-textfield-outlet-font-reset-at-layout.md`).
- Icons are SF Symbols from `Icons`, tinted by the source list through `Icons.tint(for:)`.
  Never use `NSWorkspace` document icons: their colors come from whichever app claims the
  extension, and they clash with the tinted symbols beside them.
- Decide what counts as hidden only through `HiddenFiles` (RcGitKit). The tree keeps every row
  it read and asks `BrowserModel.isVisible` at display time, so the ⇧⌘. toggle re-filters
  without a read; never drop hidden rows at load time.
- Anything that differs between the installed app and the dev bundle reads `AppBuild`
  (variant, source checkout, commit, `updateMode`, `relaunchPath`); never test the bundle
  identifier by hand. Work the updater does outside the process — toolchain probe, checkout
  read, pull, build, relaunch — goes behind `UpdateEnvironment` so `UpdateModel` stays
  testable.
- A command the user pastes or the updater runs quotes every variable word with
  `ShellWord.quote` (RcGitKit, round-trip tested through `/bin/sh`); never interpolate a path
  raw or hand-roll another quoter.
- Every step the updater runs outside the process is bounded and is logged *before* it runs,
  never after: the probe carries a timeout and a kill, the build an idle clock, a wall-clock
  limit and a cancel, every kill signals the process group, the result type has a failure case
  that says it could not answer, and the log's last line is the step that stalled. Nothing
  may have only "success" and "forever" as outcomes
  (`docs/solutions/runtime-errors/unbounded-preflight-probe-spins-with-no-step-log.md`).
- One reader per `Pipe`. Setting a `readabilityHandler` and also calling
  `readDataToEndOfFile()` reads one descriptor from two places, and the second blocks until
  EOF — which a grandchild that inherited the write end never delivers, long after
  `terminationHandler` has fired. Drain on a thread that stops at EOF, and bound that wait too.
- Only ⌘W on a single tab drops it from the next launch. `OpenTabs.record` merges the live
  tabs into the saved list and drops only its `dropping` path; ⇧⌘W (`keepingClosedTabs`) and
  ⌘Q relaunch with the same tabs. `OpenTabs.save` replaces the list and only `restoreTabs`
  calls it, to drop a repository that no longer opens.
- State the relaunched build reads — the open tabs — is handed over, not just saved:
  `OpenTabs.handOff` flushes the write because both processes are alive at the launch, and
  `AppDelegate.tabsHandedOff` stops the old process writing afterwards
  (`docs/solutions/runtime-errors/userdefaults-handover-race-self-update-relaunch.md`).
  The latch holds only once the new build has launched: `UpdateModel` calls
  `UpdateEnvironment.reclaim()` when the launch fails, which lifts it and saves the tabs again.
- `UpdateModel` runs one of two modes. `.update` (release) fetches and refuses a dirty
  checkout; `.rebuild` (dev) builds the working tree as it stands and has no up-to-date
  shortcut, because the tree moves without the commit. Do not make `.rebuild` fetch.
- A `BrowserModel` method that changes `location` — the tree browsed, the open file, the
  history scope, the detail commit — calls `noteLocation()` after the change, or brackets
  itself in `beginMove()`/`endMove()` when the change spans an await. That feeds the app-wide
  back/forward list: a move that skips it is a stop Back cannot return to, and a report from
  the middle of a move is a stop nobody was ever at.
- An action that changes the pane on its way somewhere calls `setMode`, then the model, then
  `recordStop()` once the call returns. `switchMode` is for a pane picked by itself. Two
  records for one click leave a Back press that changes nothing.
- `Navigator` (RcGitKit) owns the one `BackForwardList` and decides what it records:
  only the front tab, and nothing between `beginRestore` and `endRestore`. `AppDelegate`
  only forwards tab events to it. A stop names its tab by repository path, so closing a tab
  calls `tabClosed`; never keep a stop for a tab that is gone.
- `BrowserModel.restore` coalesces like `ChangesModel`: a restore asked for while one runs
  sets `pendingRestore` and returns. Restores and ref refreshes share one queue: a restore
  waits for `refreshRun`, and a refresh asked for during a restore sets `refreshPending` and
  runs once it ends. Never let two run at once — the second would resolve its file against a
  root the first is still loading.
- Tests use `FakeRepository` (in-memory, configured per test) for behavior and `GitFixture`
  for the real bridge; `GitFixture` builds the same standard repository as `rc-git-testkit` —
  keep those two in step.
- `xcrun swift-format lint --strict` must pass; `.swift-format` at the root is the config.
- Never collapse a view with a height-0 constraint while it has visible subviews — hide it
  inside an `NSStackView` with `detachesHiddenViews`.
- An `NSSplitView` runs in one of two modes; pick one per split and never mix them. With no
  sizing delegate methods it sizes its panes with Auto Layout, and pane floors plus
  `setHoldingPriority` hold. Implementing any of `constrainMinCoordinate`,
  `constrainMaxCoordinate`, `resizeSubviewsWithOldSize` or `shouldAdjustSizeOfSubview` switches
  it to frame mode, where a pane floor only fights the frame it is handed and loses silently —
  adding a clamp to an Auto Layout split is what broke History's drawer the third time. In
  frame mode, own `splitView(_:resizeSubviewsWithOldSize:)` and place both panes through
  `LayoutPolicy.splitPane`, put the minimum on the split itself (both floors plus the divider),
  and re-clamp in `viewDidLayout` — that is where a restored divider position first reaches the
  screen. The clamps bound a *drag* only: not `adjustSubviews`, not the autosave restore.
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
  never lands, and resets the list to `.idle` when a scan was in flight so the next switch
  back to the tab reloads rather than showing a spinner, or a report, nothing will refresh. A
  rescan keeps the last `.loaded` report (only a first scan shows `.loading`), so the commit
  and stash selections never read empty mid-scan.
