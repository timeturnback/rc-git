---
title: "NSStackView zero-height collapse crushes the commits pane, then NSSplitView autosave persists it"
date: 2026-09-07
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
symptoms:
  - "Commit list table in the history split view renders at zero height; the commits pane is fully crushed"
  - "History tab shows only the search bar and the 'Select a commit' placeholder; no rows, no draggable divider for the list"
  - "The CLI (rc-git log) returns commits for the same repository, so the data layers are fine"
  - "A build with the constraint fix still shows the crushed pane: NSSplitView autosaveName restores the broken divider position saved by the buggy build"
  - "Unified log shows nothing for the process; AppKit's constraint breakage left no observed trail"
root_cause: logic_error
resolution_type: code_fix
severity: high
framework_version: macos 26.6.2
related_components: [appkit, autolayout, nssplitview, nsstackview]
tags: [appkit, nsstackview, nssplitview, autolayout, unsatisfiable-constraints, autosavename, divider-position, compression-resistance, fill-distribution, hugging-priority, zero-height]
---

# NSStackView zero-height collapse crushes the commits pane, then NSSplitView autosave persists it

## Problem

The History pane showed no commits. The pane's vertical `NSSplitView` allocated its entire
height to the detail pane: only the search bar and the "Select a commit" placeholder were
visible, and the commit table sat in a zero-height pane above them. The data layer was fine —
`rc-git log <repo>` against the same repository returned rows — so the commits loaded and
were handed to the table; the table just had no pixels.

Three layered causes, all in `mac/Sources/RcGitApp/HistoryViewController.swift`:

1. The scope-chip bar was "hidden" by activating a `heightAnchor == 0` constraint on a stack
   whose label and ✕ button kept required vertical compression resistance — unsatisfiable, so
   AppKit broke constraints arbitrarily and the split crushed the whole commits pane.
2. `NSSplitView` autosaves divider positions under `autosaveName`. The broken build persisted
   the crushed position, so a rebuilt app restored the broken layout on every launch.
3. The stack that holds the commit list used NSStackView's default gravity-areas
   distribution, which does not reliably stretch an `NSScrollView` (no intrinsic content
   size) to fill the remaining height.

## Symptoms

- History tab renders empty: search bar at the top, "Select a commit" placeholder centered,
  no commit rows, no visible divider to drag the list back open.
- `rc-git log <repo>` on the same repository prints commits normally — the core, FFI, and
  model layers are all delivering data.
- Rebuilding with a corrected layout still shows the same empty pane (cause 2: the crushed
  divider position is restored from user defaults on launch).
- The failure survives restarts but is per-machine state, not code state — a fresh user
  account or a bumped autosave name renders correctly.

## What Didn't Work

- **Fixing only the constraint (commit `96284d1`).** The zero-height hack was replaced with
  a vertical `NSStackView` using `detachesHiddenViews`, and the scope bar switched to plain
  `isHidden`. Correct in isolation — the unsatisfiable constraint was gone — but the user
  still saw no commits: the pre-fix build had already autosaved the crushed divider position
  under `"rc-git.history"`, and the fixed build faithfully restored it. On top of that, the
  new stack kept the default gravity-areas distribution, which under-allocated the scroll
  view even where the split gave the pane room. Ruling out the data layer with the CLI is
  what kept the diagnosis on layout rather than a fruitless hunt through the log pipeline.

## Solution

Both fixes land in `mac/Sources/RcGitApp/HistoryViewController.swift`, commits `96284d1` and
`9885da1`.

**1. Hide the scope bar inside a stack instead of collapsing it with a constraint.**

Before — a stored zero-height constraint toggled on the bar while its subviews stayed
visible:

```swift
scopeBar.clipsToBounds = true
let collapsed = scopeBar.heightAnchor.constraint(equalToConstant: 0)
collapsed.isActive = true
scopeBarCollapsed = collapsed
// ... and in setScope:
scopeBarCollapsed?.isActive = path == nil
```

After — the bar is an arranged subview of a `detachesHiddenViews` stack, and hiding is just
`isHidden` (`HistoryViewController.swift:252`, `:259`):

```swift
scopeBar.isHidden = true
// ... and in setScope:
scopeBar.isHidden = path == nil
```

**2. Make the commits pane a fill-distribution stack with a height floor.** The pane is one
vertical stack — `searchArea`, `scopeBar`, `scroll` — with `distribution = .fill` and
`detachesHiddenViews` (`HistoryViewController.swift:129`). The scroll view gets the
lowest vertical hugging (`rawValue: 1`, line 132) so it soaks up all remaining height; the
bars hug at `.defaultHigh` (lines 133-134). A floor stops the split from ever crushing the
list again (`HistoryViewController.swift:148`):

```swift
scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 150),
```

The detail pane's stack gets the same treatment: `.fill` distribution (line 214), header
hugs high, split hugs at `rawValue: 1` (`HistoryViewController.swift:217-218`).

**3. Version the split autosave names.** The old names carried the crushed divider position
in user defaults, so the names moved to `.v2` (`HistoryViewController.swift:157`, `:203`):

```swift
// v2: the pre-fix build autosaved a crushed divider position under the old name.
split.autosaveName = "rc-git.history.v2"
// ...
detailSplit.autosaveName = "rc-git.history-detail.v2"
```

## Why This Works

The failure chain:

1. `scopeBar.heightAnchor == 0` at required priority fought the required vertical
   compression resistance of the bar's label and button. Auto Layout cannot satisfy both, so
   AppKit picked constraints to break — and it does not pick predictably. Here it sacrificed
   the constraints keeping the commits pane open, and the enclosing split (holding priority
   251 on the commits pane, `HistoryViewController.swift:160`) collapsed it to zero.
2. `NSSplitView` wrote that collapsed divider position to user defaults under its
   `autosaveName`. Autosave is a recorder, not a validator: it captures whatever layout
   exists, broken or not, and re-imposes it on the next launch — which is why the constraint
   fix alone changed nothing on the user's machine.
3. Even with room granted, gravity-areas distribution sizes arranged views by intrinsic
   content size and gravity, and an `NSScrollView` has no intrinsic height to claim space
   with. `.fill` distribution plus an explicit hugging gradient (scroll lowest) makes "the
   scroll takes everything the bars don't need" an actual constraint rather than a hope, and
   the `>= 150` floor makes the collapsed state unsatisfiable in the safe direction — the
   split must yield, not the list.

Renaming the autosave keys orphans the poisoned defaults instead of trying to detect and
repair them; first launch under `.v2` lays out from the code's own priorities.

## Prevention

- **Never collapse a view with a height-0 constraint while it has visible subviews.** The
  subviews' compression resistance makes the system unsatisfiable and AppKit breaks
  constraints arbitrarily — often far from the offending view. Put the view in an
  `NSStackView` with `detachesHiddenViews = true` and set `isHidden`; a detached view costs
  no height and fights nothing.
- **Give scroll content inside a split a minimum-height (or width) floor.** A scroll view
  has no intrinsic content size, so nothing else stops a split or a layout regression from
  taking it to zero. The floor turns "layout bug" into "slightly cramped pane" instead of
  "feature looks deleted."
- **Treat `NSSplitView.autosaveName` as persistent state that captures broken layouts.**
  A build that ever ran with bad layout has poisoned that key for every user who launched
  it. When the layout structure around a split changes — panes added, constraints reworked,
  a collapse bug fixed — version the name (`.v2`) so old positions are orphaned rather than
  restored. Same discipline as `NSWindow` frame autosave (see
  `../runtime-errors/appkit-swallowed-launch-exception-windowless-app.md`, where a saved
  frame kept re-triggering a launch crash).
- **Prefer explicit `.fill` distribution when a stack must stretch a scroll view**, with a
  deliberate hugging gradient: the stretchy view at rock-bottom vertical hugging
  (`rawValue: 1`), fixed bars at `.defaultHigh`. Gravity-areas distribution is for stacks of
  intrinsically-sized views, not for "one view takes the rest."
- **Rule the data layer out with the CLI before touching layout.** `rc-git log <repo>`
  against the same repository proved commits loaded fine and pinned the bug to the view in
  one command.

## Related Issues

- `../runtime-errors/appkit-swallowed-launch-exception-windowless-app.md` — sibling AppKit
  bug in this app: autosaved UI state (an `NSWindow` frame there, a split divider here)
  re-imposing a broken layout across launches.
