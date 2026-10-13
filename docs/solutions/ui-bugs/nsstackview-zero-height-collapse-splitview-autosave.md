---
title: "A split pane driven to zero height, and the NSSplitView autosave that makes it permanent"
date: 2026-09-07
last_updated: 2026-09-25
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
symptoms:
  - "A pane of a split renders at zero height; the feature in it reads as deleted, with no divider left to drag it back open"
  - "History tab shows only the search bar and the 'Select a commit' placeholder, no commit rows (2026-09-07 instance)"
  - "Clicking a changed file in the commit-detail drawer flashes 'Loading <path>…' and then renders nothing (2026-09-21 and 2026-09-25 instances)"
  - "A min/max coordinate clamp is in place and the pane is still at zero: those delegate methods bound a divider drag, nothing else"
  - "The CLI returns the same data the view is asking for, so the core, FFI, and model layers are fine"
  - "A build with the layout fix still shows the broken pane: autosaveName restores the position the buggy build saved"
root_cause: logic_error
resolution_type: code_fix
severity: high
framework_version: macos 26.6.2
related_components: [appkit, autolayout, nssplitview, nssplitviewdelegate, userdefaults]
tags:
  [
    appkit,
    nssplitview,
    nssplitviewdelegate,
    autosavename,
    divider-position,
    zero-height,
    autolayout,
    nsstackview,
  ]
---

# A split pane driven to zero height, and the NSSplitView autosave that makes it permanent

This has happened three times, in the same file, over eighteen days. Instance 2 happened *while
following the prevention rules written after instance 1*; instance 3 happened *while following
the rules written after instance 2*, on a build that carried every one of them. All three are
kept because the sequence is the argument: each remedy was real and none was sufficient, and
the one that finally makes the bad state unreachable is in the Solution's step 3 — owning the
split's resize.

## Problem

A pane of a vertical `NSSplitView` renders at zero height, so the feature inside it looks
deleted. The data layer is provably fine — the view is handed its content and has no pixels to
draw it in. Worse, `NSSplitView` writes the degenerate divider position into user defaults
under its `autosaveName` and restores it on every later launch, so fixing the layout code
changes nothing on a machine that already ran the broken build.

**Instance 1 (2026-09-07, commits pane).** The History pane's split gave its whole height to
the detail pane; the commit table sat crushed above it. Upstream cause: an unsatisfiable
`heightAnchor == 0` constraint on the scope-chip bar.

**Instance 2 (2026-09-21, commit-detail drawer).** The drawer's changed-files list rendered
fine and the diff pane below it was zero points tall. No broken constraint at all this time —
just one pane with a min-height floor and one without.

**Instance 3 (2026-09-25, commit-detail drawer again).** The same drawer, the same zero-height
diff pane, on the `.v4` build that had just fixed instance 2 — both panes floored, both
coordinate clamps implemented, both autosave names bumped. The dev domain had already recorded
`rc-git.history-detail.v4` as `763 / 0`. The clamps never ran: `constrainMinCoordinate` and
`constrainMaxCoordinate` are consulted for a user's divider drag, not for `adjustSubviews` and
not for the autosave restore. The per-pane Auto Layout floors never ran either, because a bare
`NSSplitView` hands its panes frames and those frames win.

## Symptoms

- The pane renders empty with no draggable divider to recover it. Instance 1: no commit rows,
  only the search bar and the "Select a commit" placeholder. Instance 2: clicking a changed
  file flashes "Loading `<path>`…" and then shows nothing — no diff, no error, no empty-diff
  message.
- The CLI returns the data for the same input, pinning the bug to the view:
  `rc-git log <repo>` for instance 1,
  `cargo run -q -p rc-git-cli -- diff <repo> <rev>^ <rev> <path>` for instance 2.
- The stored geometry is itself degenerate. Reading it takes one command and no app launch:

  ```
  $ defaults read com.timeturnback.rcgit.dev \
      "NSSplitView Subview Frames rc-git.history-detail.v3"
  (
      "0.000000, 0.000000, 349.000000, 864.000000, NO, NO",
      "0.000000, 101.000000, 349.000000, 0.000000, YES, NO"
  )
  ```

  Instance 3 read the same way one key later, and the shape is the tell that the clamps were
  never asked: the divider sits at the full height of the split, far past the max coordinate
  they compute.

  ```
  $ defaults read com.timeturnback.rcgit.dev \
      "NSSplitView Subview Frames rc-git.history-detail.v4"
  (
      "0.000000, 0.000000, 396.000000, 763.000000, NO, NO",
      "0.000000, 764.000000, 396.000000, 0.000000, NO, NO"
  )
  ```

  Entries are `x, y, w, h, isCollapsed, isHidden`; subview 1 is the diff scroll view, at
  height 0. The release domain `com.timeturnback.rcgit` held the same zero-height shape — and
  there, the outer split's key `rc-git.history.v3` also recorded the whole drawer at 101pt, the
  file list's 100pt floor plus a 1pt divider, i.e. the drawer starved to its own minimum. The
  `.dev` domain's outer split was healthy at 865pt, so the two bundles were poisoned to
  different depths: read both before concluding anything.
- Rebuilding with corrected layout code still shows the broken pane, because the crushed
  position is restored from user defaults on launch.
- The failure survives restarts but is per-machine state, not code state. A fresh user account
  or a bumped autosave name renders correctly — that is a diagnostic tell confirming the
  failure is persisted state, **not** a fix.
- Instance 1 left no trail in the unified log; AppKit's constraint breaking is silent.

## What Didn't Work

- **Fixing only the constraint (commit `96284d1`).** The zero-height hack was replaced with a
  vertical `NSStackView` using `detachesHiddenViews`, and the scope bar switched to plain
  `isHidden`. Correct in isolation, but the user still saw no commits: the pre-fix build had
  already autosaved the crushed divider position under `"rc-git.history"`, and the fixed build
  faithfully restored it. The new stack also kept the default gravity-areas distribution, which
  under-allocates a scroll view even where the split gives the pane room.
- **Asking the user to drag the divider back.** Offered as the workaround at the end of the
  first round. It did not resolve the report. (session history)
- **Applying this doc's own prevention rules — and still shipping the bug.** The `.v3` drawer
  rewrite (`08d943c`) followed them literally: both autosave names bumped `v2` -> `v3`, and a
  min-height floor added. The floor went on the changed-files scroll view and the diff scroll
  view got none — and that commit had just flipped `detailSplit.isVertical` from `true` to
  `false`, restacking the drawer from side-by-side to changes-above-diff and making the diff a
  height-bearing pane for the first time. Compliance was not protection.
- **Trusting an Auto Layout floor to undo a restored position.** The instinct is that
  `diffScroll.heightAnchor >= 150` is the whole fix. It is not, on a machine whose defaults are
  already poisoned: the restored position arrives as a *frame*, and a floor added afterwards
  does not push a divider back out of a position the split has already been told to take.
- **Trusting an Auto Layout floor at all, on a pane of a bare `NSSplitView` (instance 3).**
  Stronger than the point above: the floor does not govern the *computed* layout either. The
  split assigns each pane a frame, the pane's `translatesAutoresizingMaskIntoConstraints`
  turns that frame into required constraints, and `heightAnchor >= 150` is then the constraint
  the engine breaks — silently, with nothing in the log. Two panes were floored after instance
  2 and the diff pane still reached zero. The floor now sits on the split itself, where it
  asks the enclosing layout for room instead of arguing with a frame.
- **Clamping the divider with `constrainMinCoordinate`/`constrainMaxCoordinate` (instance 3).**
  This was instance 2's "durable part". It is not durable: AppKit asks those methods where a
  divider the user is *dragging* may go. Automatic resizing runs through `adjustSubviews`, and
  the autosave restores a position by setting frames; neither path consults them. They were in
  place, correct, and irrelevant while `.v4` recorded a diff pane at zero height. Keep them for
  drags; do not count them as the guard.
- **Suspecting the diff pipeline in instance 2.** A load that never lands points at core, FFI
  or `BrowserModel` — the drawer loads one blob diff per click, so a dropped generation or a
  swallowed error looks identical from outside. One CLI command moved the diagnosis to layout.

## Solution

All changes are in `mac/Sources/RcGitApp/HistoryViewController.swift`, across commits
`96284d1`, `9885da1` (instance 1) and `dd3c551` (instance 2), plus instance 3's change, which
also adds the pane arithmetic to `LayoutPolicy` (RcGitKit) with tests. Steps 1 and 4 stand as
written; steps 2 and 3 are what instance 3 rewrote.

**1. Hide a view by detaching it, never by collapsing its height.** Before — a stored
zero-height constraint toggled on a bar whose subviews stayed visible:

```swift
scopeBar.clipsToBounds = true
let collapsed = scopeBar.heightAnchor.constraint(equalToConstant: 0)
collapsed.isActive = true
scopeBarCollapsed = collapsed
// ... and in setScope:
scopeBarCollapsed?.isActive = path == nil
```

After — the bar is an arranged subview of a `detachesHiddenViews` stack, and hiding is just
`isHidden`:

```swift
scopeBar.isHidden = true
// ... and in setScope:
scopeBar.isHidden = path == nil
```

**2. Floor the split, not its panes, and keep the floors in `LayoutPolicy`.** The two heights
are thresholds, so they live where the repo's invariant says thresholds live — `LayoutPolicy`
(RcGitKit), where a test can reach them — and `HistoryViewController` aliases them. What the
constraint system is asked for is one floor on the split itself, which propagates a real
minimum up through the drawer's stack instead of contradicting a pane's assigned frame:

```swift
// LayoutPolicy:
public static let historyChangesFloor: CGFloat = 100
public static let historyDiffFloor: CGFloat = 150

// ... in buildDetailPane():
detailSplit.heightAnchor.constraint(
    greaterThanOrEqualToConstant: Self.changesPaneFloor + Self.diffPaneFloor
        + detailSplit.dividerThickness),
```

The per-pane `heightAnchor >= …` constraints from instance 2 are gone, along with
`setHoldingPriority` and the `changesScroll` hugging that biased the split's own distribution:
step 3 does that job now, and leaving either in place would mean two mechanisms disagreeing
about the same divider.

Elsewhere the "floor the axis the split can crush" rule still holds for what constraints *can*
govern: the commits pane keeps `scroll.widthAnchor >= 320`, its width being what the outer
horizontal split takes from it. The 150 matches the Changes tab's own diff scroll view
(`ChangesViewController.swift:382`), and that tab has no `NSSplitView` at all, so its panes are
ordinary subviews whose floors work as floors.

**3. Lay the panes out yourself, in `splitView(_:resizeSubviewsWithOldSize:)`, and re-clamp in
`viewDidLayout`.** This is the durable part, and what instances 1 and 2 both lacked. The
arithmetic is pure and tested (`LayoutPolicy.splitPane`, `LayoutPolicyTests`); the controller
only applies it to frames:

```swift
public static func splitPane(
    available: CGFloat, preferred: CGFloat, firstFloor: CGFloat, secondFloor: CGFloat
) -> CGFloat {
    guard available > 0 else { return 0 }
    guard available >= firstFloor + secondFloor else {
        return available * firstFloor / (firstFloor + secondFloor)
    }
    return min(max(preferred, firstFloor), available - secondFloor)
}
```

```swift
func splitView(_ splitView: NSSplitView, resizeSubviewsWithOldSize oldSize: NSSize) {
    guard splitView === detailSplit else {
        splitView.adjustSubviews()
        return
    }
    layoutDetailPanes()
}

override func viewDidLayout() {
    super.viewDidLayout()
    layoutDetailPanes()
}
```

`layoutDetailPanes` reads the current top-pane height as `preferred`, so a user's divider
survives every resize, and writes a frame only when it differs, so the pass converges instead
of looping. `viewDidLayout` is the half that repairs a poisoned autosave: the restore arrives
as frames during the first layout, and the clamp rewrites them on the way out. A drawer too
short for both floors splits what there is in proportion rather than zeroing a pane, which
keeps the divider reachable at any window size.

The two coordinate clamps stay, now correctly scoped to what they actually do — bound a drag:

```swift
func splitView(
    _ splitView: NSSplitView, constrainMaxCoordinate proposedMaximumPosition: CGFloat,
    ofSubviewAt dividerIndex: Int
) -> CGFloat {
    guard splitView === detailSplit else { return proposedMaximumPosition }
    let lastOpen =
        splitView.bounds.height - splitView.dividerThickness - Self.diffPaneFloor
    return min(proposedMaximumPosition, max(Self.changesPaneFloor, lastOpen))
}
```

The identity `guard` keeps the floors the drawer's own: the controller owns a second, outer
split (the commit list beside the drawer) that takes no delegate for its coordinates, and the
guard is what stops these clamps — and the manual layout — leaking onto it. That guard is also
why `resizeSubviewsWithOldSize` calls `adjustSubviews()` for any other split: implementing the
method opts *every* split with this delegate out of automatic resizing.

**4. Version the split autosave names, to discard the poison already stored.** The outer split
is at `.v4` (its stored geometry was healthy at instance 3, so it was left alone); the drawer
is at `.v5`, having passed through `.v2` after instance 1, `.v3` at the drawer rewrite and
`.v4` at instance 2:

```swift
// v5: v4 parked the divider at the bottom again, leaving the diff pane at zero height.
// The delegate below now lays the panes out itself, so no position reaches that state.
detailSplit.autosaveName = "rc-git.history-detail.v5"
```

Step 3 repairs a poisoned key on its own, so the bump is only about the first launch after the
fix: without it the repaired position is the old one clamped — a 613pt file list above a 150pt
diff — instead of the 100pt list and full-height diff a fresh key gives.

**Verification:** `swift build`, `xcrun swift-format lint --strict`, and the RcGitKit suite —
170 tests, including six new `LayoutPolicy.splitPane` cases, one of them the `763 / 0` geometry
read out of the poisoned defaults. The data layer was cleared first with
`cargo run -q -p rc-git-cli -- diff <repo> 9b51d13^ 9b51d13 <path>`, which printed the hunks
the drawer had no pixels to show.

## Why This Works

The failure chain, instance 1:

1. `scopeBar.heightAnchor == 0` at required priority fought the required vertical compression
   resistance of the bar's label and button. Auto Layout cannot satisfy both, so AppKit picked
   constraints to break — and it does not pick predictably. Here it sacrificed the constraints
   keeping the commits pane open, and the enclosing split collapsed it to zero.
2. Gravity-areas distribution sizes arranged views by intrinsic content size, and an
   `NSScrollView` has no intrinsic height to claim space with. `.fill` plus an explicit hugging
   gradient makes "the scroll takes everything the bars don't need" an actual constraint rather
   than a hope.

Instance 2 needed neither of those. With a floor on one pane and none on the other, the split
had a perfectly legal layout in which the file list took its 100pt minimum and the diff took
everything left — including nothing. The drawer laid out at least once with nothing to spare,
the divider went to the bottom, and the diff pane went to zero. No constraint was broken; the
layout was simply unconstrained in the direction that hurt.

In both, the same second stage made it permanent: `NSSplitView` wrote that position to user
defaults under its `autosaveName`. **Autosave is a recorder, not a validator.** It captures
whatever geometry exists, sane or not, and re-imposes it on the next launch — which is why the
constraint fix alone changed nothing on the user's machine.

Instance 3 is what happens when the fix addresses neither stage. A bare `NSSplitView` does not
ask Auto Layout where to put its panes; it assigns each one a frame, and with
`translatesAutoresizingMaskIntoConstraints` left on — the default for a split's subviews —
that frame becomes required constraints. A `heightAnchor >= 150` added alongside is simply the
weaker half of a contradiction, and AppKit breaks it without a word. So the pane floors never
governed the computed layout. And the coordinate clamps never saw the restore: AppKit asks
those methods where a divider *being dragged* may go. `adjustSubviews` does not consult them,
and neither does the autosave, which restores by setting frames. Both mechanisms were present
and correct in the `.v4` build, and neither was ever on the path that produced `763 / 0`.

Owning `splitView(_:resizeSubviewsWithOldSize:)` puts the policy on the one path every resize
takes, and `viewDidLayout` covers the restore, which reaches the screen through a layout pass
rather than a resize. Between them there is no way for the split to display a geometry the
policy did not produce — which is the property the earlier fixes only appeared to have.

The version bump is the weakest of the four. Renaming `autosaveName` orphans a poisoned key; it
never repairs one, and it only buys one clean launch. `.v2` was set by the 2026-09-07 fix and
replaced by `.v3` at the drawer rewrite; `.v3` was poisoned within days, and `.v4` — the fix for
that — was poisoned the same way. A name bump is a reset button. The layout the app owns is the
guard, and it is the first version of this fix that also *repairs* a key instead of abandoning
it.

In instance 2 the "Loading…" flash was visible only by accident of where the placeholder lives:
`diffPlaceholder` is centered in the drawer's container, not inside the diff scroll view, so it
drew across the whole drawer while `renderDiff` a moment later wrote real hunks into a view with
no pixels. The user saw the loading state and never the result.

## Prevention

- **Never collapse a view with a height-0 constraint while it has visible subviews.** The
  subviews' compression resistance makes the system unsatisfiable and AppKit breaks constraints
  arbitrarily — often far from the offending view. Put the view in an `NSStackView` with
  `detachesHiddenViews = true` and set `isHidden`; a detached view costs no height and fights
  nothing.
- **Own the layout of a bare `NSSplitView`: implement
  `splitView(_:resizeSubviewsWithOldSize:)`, place both panes from one pure function, and call
  the same function from `viewDidLayout`.** That pair covers every path a geometry can reach
  the screen by — automatic resizing and the autosave restore — and it is the only arrangement
  of the three attempts here that ever held. Keep the function pure and in `LayoutPolicy` so a
  test pins the floors; the controller should do nothing but turn its number into frames.
- **A constraint on a pane of a bare split is not a floor.** The split assigns pane frames and
  `translatesAutoresizingMaskIntoConstraints` makes them required, so `heightAnchor >= n` is the
  side of the contradiction AppKit silently breaks. Put the minimum on the split itself, where
  it propagates outward and contradicts nothing. Panes of an `NSStackView`, or plain subviews,
  are a different story — the Changes tab's `>= 150` floors work because nothing hands those
  views frames.
- **`constrainMinCoordinate`/`constrainMaxCoordinate` bound a drag and nothing else.** Not
  `adjustSubviews`, not the autosave restore. They are worth implementing for what they cover,
  and they are never the guard. An `NSSplitViewController` whose items set `minimumThickness`
  *is* covered without any of this — `MainWindowController` sets it on both items of
  `outerSplit`, and that is the cheapest way out of this whole class of bug when a split can be
  one.
- **Re-audit whenever a pane is added or a split is restructured.** A restructure flips which
  axis is at risk, which is the whole story of `08d943c`: it flipped both splits' orientation at
  once, correctly swapping the commits pane's height floor for a width one, and leaving the diff
  pane unprotected on the height axis it had just started bearing. Neither pane was new; the
  geometry was.
- **Diagnose a suspected autosave poisoning with one command, before touching layout code:**
  `defaults read <bundle-id> "NSSplitView Subview Frames <autosaveName>"`. A height of 0 or
  `isCollapsed YES` separates "code bug" from "poisoned user defaults" instantly, with no app
  launch — which matters because the working agreement forbids launching the app to verify.
  Check both bundle ids: `com.timeturnback.rcgit` and `com.timeturnback.rcgit.dev` keep separate
  defaults, so a release-only or dev-only report is not a contradiction. The same read on an
  enclosing split tells you whether the container was starved too — a recorded height equal to a
  child's floor plus the divider means that pane sized to its own minimum.
- **Versioning `autosaveName` is a reset, not a fix, and it does not survive the next structural
  rewrite.** It orphans the bad key once. If the collapse is still reachable, the new name is
  poisoned the first time a user hits it. Bump the name *and* close the hole in the same change.
  Same discipline as `NSWindow` frame autosave (see
  `../runtime-errors/appkit-swallowed-launch-exception-windowless-app.md`, where a saved frame
  kept re-triggering a launch crash).
- **Prefer explicit `.fill` distribution when a stack must stretch a scroll view**, with a
  deliberate hugging gradient: the stretchy view at rock-bottom vertical hugging (`rawValue: 1`),
  fixed bars at `.defaultHigh`. Gravity-areas distribution is for stacks of intrinsically-sized
  views, not for "one view takes the rest."
- **Rule the data layer out with the CLI before touching layout.** `rc-git log <repo>` and
  `rc-git diff <repo> <rev>^ <rev> <path>` each pinned their instance to the view in one command.
- **A placeholder that lives in a different view from the content it replaces can lie.** A
  flash-then-nothing symptom *with* a visible loading state is evidence the load worked and the
  render target is the problem.
- **Know every autosaving split in the app, and fix them as a set.** There are exactly three
  `autosaveName` sites: the two in `HistoryViewController` (`rc-git.history.v5`, the commit
  column beside the diff, and `rc-git.history-column.v1`, the commit list over the detail; the
  controller is the delegate of both and places their panes through `LayoutPolicy.splitPane`)
  and `MainWindowController` (`rc-git.main.v2`, an `NSSplitViewController` covered by
  `minimumThickness`). The Changes tab is *not* one of them
  despite being modeled on `HistoryViewController` — it has no `NSSplitView` at all, so its
  `>= 150` floors are ordinary constraints on plain subviews. A fix applied to only one site is
  the shape of this recurrence.

## Related Issues

- `../runtime-errors/appkit-swallowed-launch-exception-windowless-app.md` — the sibling AppKit
  bug in this app: autosaved UI state re-imposing a broken layout across launches (an `NSWindow`
  frame there, a split divider here). Its framing — treat the autosave call as an active restore
  that resizes at the call site, not as passive bookkeeping — is the same idea this doc applies
  to `NSSplitView`.
- `nsstackview-hidden-at-attach-never-gets-width-alignment.md` — the other half of
  `NSStackView`'s sharp edges: a constraint you assumed the stack made was never made at all,
  where here a constraint you did make does not reach the frame the autosave restores.
- `../runtime-errors/userdefaults-handover-race-self-update-relaunch.md` — same defaults domain,
  same `defaults read` as the proving instrument. Its point that state is scoped by
  `CFBundleIdentifier` is why the dev bundle carries its own independently poisoned split key.
