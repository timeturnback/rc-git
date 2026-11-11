---
title: "A view added to an NSStackView while hidden never gets the stack's width alignment"
date: 2026-09-20
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
symptoms:
  - "The Changes section in the sidebar is narrower than the sidebar and sits against its right edge"
  - "The narrow width tracks the content: an empty repository shows a ~147pt column, one with long stash messages a ~221pt one"
  - "The ref list above it fills the sidebar normally"
  - "No unsatisfiable-constraint log: nothing is broken, a constraint was simply never made"
root_cause: api_misuse
resolution_type: code_fix
severity: medium
framework_version: macos 26.6.2
related_components: [appkit, autolayout, nsstackview]
tags: [appkit, nsstackview, autolayout, detacheshiddenviews, alignment, width, sidebar, hidden-at-attach]
---

# A view added to an NSStackView while hidden never gets the stack's width alignment

## Problem

`SidebarViewController` stacks the mode's section under the ref list and swaps sections with
`isHidden` on a stack with `detachesHiddenViews = true`. The stack uses `alignment = .width`,
so every arranged view is meant to be as wide as the stack.

The Changes section was not. It rendered as a narrow column pinned to the sidebar's right
edge, its width set by whatever its widest row happened to need — the Apply/Pop/Drop row in a
repository with no stashes, the "1 of 1 selected · Stash · Commit" row in one with a modified
file. The ref list above it, which is never hidden, filled the sidebar correctly.

`SidebarViewController.add(section:)` hid each section before handing it to the stack:

```swift
section.isHidden = true
stack.addArrangedSubview(section)
```

With `detachesHiddenViews`, a hidden arranged view is not in the view hierarchy, so the stack
never laid the section out and never created its `.width` alignment constraint. Unhiding the
section re-attaches it but does not make the stack create that constraint afterwards. The
section had no width constraint at all, so Auto Layout sized it to its content's own minimum.

## Symptoms

- The sidebar's Changes section is much narrower than the sidebar and hugs its trailing edge;
  the 8pt gutter is correct on the right and enormous on the left.
- Its width changes with the content rather than with the sidebar: the same window shows a
  different gap in two repositories.
- The ref list above the section is full width.
- Console is silent. The layout is satisfiable — one constraint was never created.

## Diagnosis

A standalone AppKit probe reproduces it in three cases against a 253pt stack, with the pane
sized by a 75pt button row:

| Case | Pane frame |
|---|---|
| Added hidden, then shown | `x=178, width=75` |
| Added visible, then hidden, then shown | `x=0, width=253` |
| Added visible, hidden before any layout, then shown | `x=178, width=75` |

So it is not "hidden at some point" that loses the width — it is never having been laid out
while attached. Hiding a view that the stack has already laid out is safe; a view that is
hidden before the stack's first layout pass never receives an alignment constraint, and the
stack does not backfill one when the view returns.

## Solution

Give every view showing its width on every show, from `SidebarViewController.show(section:)`.
The section is not the only view at risk: a window that opens on Files hides the ref list
before the stack's first layout too. Since this fix the layout has moved the sections under
`RefBannerView`, which stands in for the ref list beside a section, so the ref list toggles as
well:

```swift
NSLayoutConstraint.deactivate(widths)
for candidate in sections {
    candidate.isHidden = candidate !== section
}
let besideASection = section != nil
refs.view.isHidden = besideASection
bannerRow.isHidden = !besideASection
widths = stack.arrangedSubviews.filter { !$0.isHidden }.map {
    $0.widthAnchor.constraint(equalTo: stack.widthAnchor)
}
NSLayoutConstraint.activate(widths)
```

The constraints are re-made rather than kept because AppKit drops an ancestor's constraints for
a view it detaches, and hiding a view detaches it. Unhiding re-attaches the view
synchronously, so the constraints can be activated in the same pass.

Inside the section, `ChangesViewController.show(_:visible:width:)` does the same for every row
that toggles — the scan-truncation note, which is hidden before its stack is built, and the
stash group, which is hidden in a repository with no stashes. A row that toggles goes through
that helper; a row that is always there takes its width from the stack.

## Prevention

- Hiding a view before the stack's first layout is allowed, but a view hidden, or added
  hidden, before an `NSStackView` with `detachesHiddenViews` first lays it out gets no `.width`
  alignment constraint. The code that shows it must re-make its width on every show; never
  assume an unhide restores the stack's alignment constraints (`.claude/rules/swift.md`, the
  bullet "A view the stack has never laid out while attached").
- Sizing that depends on content when it should depend on the container is the tell: measure
  the frame in a probe before reaching for hugging and compression priorities, which cannot
  fix a constraint that does not exist.

## Related

- `docs/solutions/ui-bugs/nsstackview-zero-height-collapse-splitview-autosave.md` — the other
  half of this API's sharp edges: collapsing with a height constraint instead of `isHidden`.
