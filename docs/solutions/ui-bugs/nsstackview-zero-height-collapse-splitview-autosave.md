---
title: "A split pane driven to zero height, and the NSSplitView autosave that makes it permanent"
date: 2026-09-07
last_updated: 2026-09-21
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
symptoms:
  - "A pane of a split renders at zero height; the feature in it reads as deleted, with no divider left to drag it back open"
  - "History tab shows only the search bar and the 'Select a commit' placeholder, no commit rows (2026-09-07 instance)"
  - "Clicking a changed file in the commit-detail drawer flashes 'Loading <path>…' and then renders nothing (2026-09-21 instance)"
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

This has happened twice, in the same file, two weeks apart. The second time it happened
*while following the prevention rules written after the first time*. Both instances are kept
here because the pair is the argument: the remedy those rules prescribe is real but partial,
and the durable one is in the Solution's step 3.

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
- **Suspecting the diff pipeline in instance 2.** A load that never lands points at core, FFI
  or `BrowserModel` — the drawer loads one blob diff per click, so a dropped generation or a
  swallowed error looks identical from outside. One CLI command moved the diagnosis to layout.

## Solution

All changes are in `mac/Sources/RcGitApp/HistoryViewController.swift`, across commits
`96284d1`, `9885da1` (instance 1) and `dd3c551` (instance 2, local on `master` and unpushed as
of this writing, so that hash may be rewritten).

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

**2. Give every pane of the split its own floor, with `.fill` distribution.** Each pane is a
vertical stack with `distribution = .fill` and `detachesHiddenViews`, and the stretchy member
takes the lowest vertical hugging (`rawValue: 1`) so it soaks up remaining height while the bars
hug at `.defaultHigh`. That member is the scroll view in the commits pane but the split itself
in the drawer, where `changesScroll` is deliberately at `.defaultHigh` — port the gradient, not
the literal view type. Both floors of the detail drawer are named constants
(`HistoryViewController.swift:28-29`), applied in `buildDetailPane()` (`:255-257`):

```swift
/// The height neither pane of the detail drawer may drop below. A pane at zero reads as a
/// missing feature, and `NSSplitView` autosave records the zero and re-imposes it.
private static let changesPaneFloor: CGFloat = 100
private static let diffPaneFloor: CGFloat = 150

// ... in buildDetailPane():
changesScroll.heightAnchor.constraint(
    greaterThanOrEqualToConstant: Self.changesPaneFloor),
diffScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: Self.diffPaneFloor),
```

The commits pane's own floor changed axis at the drawer rewrite: it was
`scroll.heightAnchor >= 150` while the detail sat *below* it, and `08d943c` replaced that with
`scroll.widthAnchor >= 320` when the pane became the left side of a horizontal split, where
height comes from the split and width is the dimension that can be crushed. **Floor the axis the
split can actually take from you** — a vertical split takes height, a horizontal one takes
width. The 150 matches the Changes tab's own diff scroll view
(`ChangesViewController.swift:382`), though note that tab's panes are plain `addSubview`
children rather than split panes, so it is precedent for the value, not for the split-pane rule.

**3. Clamp the divider through `NSSplitViewDelegate`.** This is the durable part, and the one
thing the 2026-09-07 fix lacked. The controller conforms, the detail split takes it as its
delegate (`HistoryViewController.swift:224`), and the two coordinate methods make the bad
position unreachable (`:675-696`):

```swift
// MARK: NSSplitViewDelegate

/// Keeps the drawer's divider inside the two floors. Auto Layout alone cannot do this: a
/// restored autosave position is applied as a frame, and a pane parked at zero height
/// looks to the user like the diff feature is missing.
func splitView(
    _ splitView: NSSplitView, constrainMinCoordinate proposedMinimumPosition: CGFloat,
    ofSubviewAt dividerIndex: Int
) -> CGFloat {
    guard splitView === detailSplit else { return proposedMinimumPosition }
    return max(proposedMinimumPosition, Self.changesPaneFloor)
}

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

The identity `guard` keeps the floors the drawer's own: the controller owns a second,
outer split (the commit list beside the drawer) that takes no delegate today, and the guard is
what stops these clamps leaking onto it if it ever does. The max clamp keeps `changesPaneFloor`
as a lower bound on `lastOpen`, so a drawer too short to hold both floors still yields a sane
coordinate instead of a negative one.

**4. Version the split autosave names, to discard the poison already stored.** The names are at
`.v4` today (`HistoryViewController.swift:173`, `:223`), having passed through `.v2` after
instance 1 and `.v3` at the drawer rewrite:

```swift
// v4: v3 autosaved the drawer at the height of its own minimum, which is what starved
// the detail split underneath it.
split.autosaveName = "rc-git.history.v4"
// ...
// v4: v3 autosaved a divider parked at the bottom, leaving the diff pane at zero
// height on every launch. The delegate below keeps that position unreachable now.
detailSplit.autosaveName = "rc-git.history-detail.v4"
```

**Verification:** `scripts/check.sh`, the repo's full gate — cargo clippy, cargo test,
swift-format lint, 155 Swift tests — passed.

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

Why the delegate, and not just the floor: an Auto Layout floor constrains what layout the
system may *compute*. A restored autosave position is not computed — it is applied as a frame,
from user defaults, on the way up. `constrainMinCoordinate`/`constrainMaxCoordinate` are the
hook `NSSplitView` consults about where a divider is *allowed* to be, so they bound the restored
position as well as every drag afterwards. That is what makes the bad state unreachable rather
than merely un-recreatable: it holds regardless of what a user's defaults already contain.

And the version bump is the weakest of the four. Renaming `autosaveName` orphans a poisoned
key; it never repairs one, and it only buys one clean launch. `.v2` was set by the 2026-09-07
fix and replaced by `.v3` at the drawer rewrite — and `.v3` was poisoned in turn, within days,
because the rewrite re-opened a way to reach the collapse. A name bump is a reset button. The
delegate is the guard.

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
- **Floor every pane of a split, on the axis that split can crush, and re-audit the floors
  whenever a pane is added or the split is restructured.** A split is only as safe as its
  least-floored pane. This rule in its singular form ("give scroll content in a split a floor")
  is what `08d943c` satisfied by flooring one of two panes. A vertical split takes height, a
  horizontal one takes width, and a **restructure flips which axis is at risk** — which is the
  whole story of that commit: it flipped both splits' orientation at once, correctly swapping the
  commits pane's height floor for a width one, and leaving the diff pane with no floor on the
  height axis it had just started bearing. Neither pane was new; the geometry was.
- **An Auto Layout floor is not sufficient against autosave; pair it with `NSSplitViewDelegate`
  min/max coordinate clamping.** The floor governs computed layout; the delegate governs the
  divider position, including the one restored from user defaults, which arrives as a frame and
  is not re-derived from constraints. Only the delegate makes a bad divider position unreachable
  regardless of what a user's defaults hold. An `NSSplitViewController` whose items set
  `minimumThickness` is already covered by that mechanism and needs no delegate —
  `MainWindowController` sets it on both items of `outerSplit`.
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
  `autosaveName` sites: `HistoryViewController:173` and `:223` (both fixed here) and
  `MainWindowController:346` (`rc-git.main.v2`, an `NSSplitViewController` covered by
  `minimumThickness`). The Changes tab is *not* one of them despite being modeled on
  `HistoryViewController` — it has no `NSSplitView` at all, so its `>= 150` floors are ordinary
  constraints on plain subviews and need no delegate. A fix applied to only one site is the shape
  of this recurrence.

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
