---
title: "A label showing an attributed string ignores the line break mode set on the field"
date: 2026-09-20
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
symptoms:
  - "Long paths in the Changes list are cut off mid-word with no ellipsis"
  - "The field sets lineBreakMode = .byTruncatingHead and the text still clips"
  - "The tick box and the path label in the same row sit at different heights"
root_cause: api_misuse
resolution_type: code_fix
severity: low
framework_version: macos 26.6.2
related_components: [appkit, nstextfield, nstableview, nsoutlineview]
tags: [appkit, nstextfield, attributedstring, paragraphstyle, linebreakmode, truncation, nstablecellview, alignment, changes-list]
---

# A label showing an attributed string ignores the line break mode set on the field

## Problem

The Changes list renders each row as a colored status letter followed by the path, so the
label carries an `NSAttributedString` rather than a plain string. The cell set its truncation
the usual way:

```swift
let field = NSTextField(labelWithAttributedString: text)
field.lineBreakMode = .byTruncatingHead
```

Paths wider than the sidebar were cut at the column edge with no ellipsis, so
`mac/Sources/RcGitApp/` and `mac/Sources/RcGitKit/` rendered as the same unreadable stub.
`lineBreakMode` on the field is the cell's own paragraph handling, and it applies to
`stringValue`. An attributed string brings its own attributes, and drawing follows those: with
no `.paragraphStyle` in the string, the text has no truncation behavior to apply and clips.

The same rows had a second defect with the same shape. The tick lived in a 24pt table column
and the label in the next column, each returned bare as its own cell view. Neither carried a
vertical constraint, so a 14pt checkbox and an 11pt label each sat wherever their own sizing
put them in a 17pt row, a point or two apart.

## Symptoms

- Long paths clip mid-word. No ellipsis appears, in either truncation direction.
- Setting `lineBreakMode` on the field, or on `field.cell`, changes nothing.
- The tick box and its label are visibly off each other's centre line down the list.
- No constraint or console warning: nothing is unsatisfiable, the attribute is simply absent.

## Solution

Put the truncation in the string, not on the field:

```swift
let paragraph = NSMutableParagraphStyle()
paragraph.lineBreakMode = node.name.contains("/") ? .byTruncatingHead : .byTruncatingTail
let text = NSMutableAttributedString(
    string: letter + "  ",
    attributes: [.font: …, .foregroundColor: color, .paragraphStyle: paragraph])
```

Every run of the string needs the attribute — a run appended without it keeps the default
behavior for its own range.

For the alignment, one cell view owns both controls and centres them
(`ChangesViewController.swift:13`):

```swift
NSLayoutConstraint.activate([
    tick.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
    tick.centerYAnchor.constraint(equalTo: centerYAnchor),
    label.leadingAnchor.constraint(equalTo: tick.trailingAnchor, constant: 4),
    label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -2),
    label.centerYAnchor.constraint(equalTo: centerYAnchor),
])
```

The label also needs `setContentCompressionResistancePriority(.defaultLow, for: .horizontal)`,
or it holds its full intrinsic width and the truncation never triggers.

## Why This Works

`NSTextFieldCell` exposes `lineBreakMode`, `alignment`, and `font` as conveniences that build
a paragraph style for the plain string it draws. Hand it an attributed string and those
conveniences are bypassed: the string's own attributes are what `NSStringDrawing` reads. The
field looks correctly configured in code and in the debugger, which is why this reads as a
layout problem rather than a missing attribute.

Two self-sizing views in two columns have nothing tying them together vertically. A row is one
cell view with constraints, or it is two controls that happen to look aligned at one font size.

## Prevention

- Reach for `.paragraphStyle` whenever a cell shows an attributed string. Truncation,
  alignment, and line spacing set on the field are dead code there — delete them rather than
  leaving them as a claim the code does not honor.
- Never split a row's controls across table columns to line them up. One `NSTableCellView`
  with `centerYAnchor` constraints per control is the unit that stays aligned across fonts,
  row size styles, and accessibility text sizes.
- Clipping with no ellipsis is the tell for this bug. Text that truncates with an ellipsis at
  the wrong end is a paragraph-style value; text that just stops is a paragraph style that is
  not there at all.
- Verification is the user's launch, not CI: the app is never started from an agent session
  (`CLAUDE.md`, working agreement), so these fixes ship on the structural argument above and
  are confirmed on the next run of `RCGit.app`.

## Related

- `docs/solutions/ui-bugs/nsstackview-hidden-at-attach-never-gets-width-alignment.md` — the
  other missing-attribute failure in this sidebar: a constraint AppKit never created, silent
  in the same way.
