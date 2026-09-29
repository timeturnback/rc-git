---
title: "A font set on a table cell's textField outlet is reset at layout: the checked-out branch never shows bold"
date: 2026-09-29
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
symptoms:
  - "The checked-out branch in the sidebar's ref list renders in the same regular weight as every other branch, though viewFor sets a bold font on the cell's textField"
  - "Nothing logs, and reading the font right after viewFor or in didAdd still reports the bold face"
  - "The font reads regular only after the window displays and the cell lays out"
  - "Re-applying the weight from a rowSizeStyle didSet on the cell still ends regular"
  - "Labels set through attributedStringValue in the same kind of list keep their own fonts"
root_cause: api_misuse
resolution_type: code_fix
severity: low
framework_version: macos 26.6.2
related_components: [appkit, nstablecellview, nsoutlineview, nstextfield]
tags: [appkit, nstablecellview, rowsizestyle, source-list, textfield-outlet, font-weight, type-select, headless-probe]
---

# A font set on a table cell's textField outlet is reset at layout: the checked-out branch never shows bold

## Problem

The ref list marks the checked-out branch by weight. Before the fix,
`outlineView(_:viewFor:item:)` in `RefsViewController` set that weight on the cell's
`textField` outlet:

```swift
cell.textField?.stringValue = ref.shortName
let isHead = head?.refName == ref.fullName
cell.textField?.font =
    isHead
    ? NSFont.boldSystemFont(ofSize: NSFont.systemFontSize)
    : NSFont.systemFont(ofSize: NSFont.systemFontSize)
```

In the running app, with HEAD on `master`, `master` drew in the same regular weight as every
other branch. The font was right when the delegate returned the cell. `NSTableCellView` then
replaced it: during layout it re-applies a plain system font, sized by the cell's
`rowSizeStyle`, to whatever view is its `textField` outlet.

This is not specific to source lists. This session's probe set a font on a plain-string
`textField` outlet in every table style (`.automatic`, `.inset`, `.fullWidth`, `.plain`,
`.sourceList`): at `.default` and `.small` row sizes each one came back as the regular system
font at the row size's point size, and only a table whose `rowSizeStyle` is `.custom` kept the
font it was given.

## Symptoms

- The checked-out branch shows in regular weight. The bold never appears.
- Reading `cell.textField?.font` right after `viewFor` gives the bold face, so the code looks
  correct in the debugger.
- The font is still the one you set in `outlineView(_:didAdd:forRow:)`, after the table has
  already set the cell's `rowSizeStyle` (this session's probe).
- Nothing logs: no constraint warning, no exception.
- Only a plain `stringValue` loses its styling. A label that carries an `NSAttributedString`
  keeps its fonts.

## What Didn't Work

- **Re-applying the weight from `rowSizeStyle`'s `didSet`, with the label still the
  `textField` outlet.** This session's probe still ended regular. The table sets the row size,
  then AppKit re-styles the outlet again in layout, after the setter has run.
- **Re-applying the font in `layout()`.** Rejected before shipping. AppKit resets the outlet on
  every layout pass, and each font change invalidates the label's intrinsic size, so the cell
  and AppKit would take turns scheduling layout passes.

## Solution

Take the label off the outlet and own its font. `SidebarCellView`
(`mac/Sources/RcGitApp/Style.swift:48`) keeps the symbol as `imageView`
(`Style.swift:76`) and never assigns `label` (`Style.swift:52`) to `textField`. It sets the
label's font itself, from the two places its inputs change (`Style.swift:58`, `Style.swift:97`):

```swift
var isEmphasized = false {
    didSet { applyRowSize() }
}

override var rowSizeStyle: NSTableView.RowSizeStyle {
    didSet { applyRowSize() }
}
```

`applyRowSize()` (`Style.swift:132`) picks 11, 13, or 15 pt from `Style.fontSize(for:)`
(`Style.swift:25`), semibold when emphasized, and sizes the badge, symbol, and symbol column
to match. The ref list now writes to the cell's own properties
(`mac/Sources/RcGitApp/RefsViewController.swift:366`):

```swift
let cell = refCell()
cell.imageView?.image = Icons.symbol(for: ref.kind)
cell.label.stringValue = ref.shortName
cell.isEmphasized = head?.refName == ref.fullName
```

The symbol stays on `imageView` because the source list tints that outlet's template image
with the accent color. The running app showed accent-blue ref icons with the outlet set; an
offscreen `cacheDisplay` render could not show tint either way.

With the label off the outlet, `textField` is nil. The SDK headers document only the cell-based
type-select default, so the view-based default is not something to lean on for these rows. Both
tables that show these cells answer it through the delegate instead:
`outlineView(_:typeSelectStringFor:item:)` at `RefsViewController.swift:378` and
`mac/Sources/RcGitApp/TreeViewController.swift:296`. The tree needs it now because its old
cells put an attributed string on the outlet, which kept both the styling and the free
type-select; moving it to `SidebarCellView` gave up the second.

The fix is on `master`.

### Checking it without launching the app

The project never launches the app to verify a change (`CLAUDE.md`, working agreement). An
offscreen harness reproduces the reset and confirms the fix. `Style.swift` imports only
AppKit, so it compiles into a standalone binary:

```sh
xcrun swiftc -o harness main.swift mac/Sources/RcGitApp/Style.swift && ./harness
```

`main.swift` builds a one-column `NSOutlineView`, passes it through `Style.sidebarList`, puts
it in an `NSScrollView` inside an `NSWindow` that is never ordered front, calls
`reloadData()` and `window.displayIfNeeded()`, then `layoutSubtreeIfNeeded()` on each cell
the delegate returned, and prints the label's font. `SidebarCellView` is main-actor isolated,
so mark the delegate class and the entry function `@MainActor` and call it inside
`MainActor.assumeIsolated { }`. Keep the file named `main.swift`: in a multi-file compile,
swiftc accepts top-level statements only there.

The probe, rerun against the committed code at medium row size:

- Plain `NSTableCellView`, label assigned to `textField`, bold set in `viewFor`:
  `.AppleSystemUIFontBold` when `viewFor` returns, `.SFNS-Regular` 13 pt after display and
  layout.
- `SidebarCellView` with `isEmphasized = true`: `.AppleSystemUIFontDemi` 13 pt after display
  and layout.

### Related measurements from the same probes

These feed the tree's width fit, `reportContentWidth` (`TreeViewController.swift:112`):

- An `NSTextField` label's alignment rect is its text width. The padding its cell draws
  (`alignmentRectInsets` 2 pt left and right) falls outside it. So
  `SidebarCellView.contentWidth` (`Style.swift:110`) — symbol column + 6 + rounded-up text +
  6 + badge + 2 — equals the cell's `fittingSize.width`: 77/77 for `feature`, 126/126 for
  `rc-git-highlight` at medium. It measures the regular weight; an emphasized `master` fits at
  78.5 against 76. Only the ref list emphasizes, and only the tree sizes itself by
  `contentWidth`.
- A `.sourceList` outline starts a level-0 cell at x = 25 (the 10 pt row inset plus the
  disclosure triangle), 14 pt further per level with `indentationPerLevel` 14
  (`TreeViewController.swift:54`). Once the column fills the list, it keeps 16 pt between the
  cell's trailing edge and the list's. AppKit publishes neither.
  `cellTrailingInset` (`TreeViewController.swift:105`) records the 16; `reportContentWidth`
  reads each cell's start from `frameOfCell(atColumn:row:)` (`TreeViewController.swift:121`)
  rather than hard-coding 25.
- Source-list row heights offscreen on macOS 26.6.2, built against the macOS 27.0 SDK: 24, 32,
  and 40 pt for small, medium, and large (this session's probe).

## Why This Works

`NSTableCellView.h:39` states the contract. The table sets the cell's `rowSizeStyle` to its
own `effectiveRowSizeStyle`, and then "The cell view will layout the textField and imageView
based on the rowSizeStyle." Layout here includes the font. A second probe this session showed
it: a plain label cell with no constraints got 11, 13, and 15 pt per row size, and its
`imageView` was positioned per row size too.

The outlets are the part of the cell AppKit owns. Any per-row styling written to one lasts
until the next layout pass. A label that is not an outlet is an ordinary subview: AppKit still
forwards `backgroundStyle` to it (`NSTableCellView.h:35`), so it turns white on a selected
row, but it never resets its font. The cell restyles it from two inputs: the row size, which
AppKit's reset used, and whether the row is HEAD, which the old code used. Each input's setter
re-applies both, so the order in which the table and the delegate set them does not matter.

A table can also keep outlet fonts by setting `rowSizeStyle = .custom`, which the probe
confirmed. The sidebar does not take that route: its rows follow the system's sidebar size,
and `.custom` gives that up.

An attributed string survives because what gets reset is the field's `font` property, and the
string's own `.font` attributes win when it draws. This session's probe, with its own strings,
kept a monospaced-bold letter and a medium-weight path through layout in both `.automatic` and
`.sourceList` styles. That is why `ChangeCellView` can still assign its label to `textField`
(`mac/Sources/RcGitApp/ChangesViewController.swift:27`): it sets `attributedStringValue`
(`ChangesViewController.swift:777`). The ref list's group header also keeps the outlet on
purpose (`RefsViewController.swift:409`); there, the source list's own header styling is what
is wanted.

### Known instances not fixed by this change

The same reset hits cells outside the sidebar that still style a plain-string outlet:

- `mac/Sources/RcGitApp/HistoryViewController.swift:841` sets a monospaced font on the commit
  hash column, in a table at `.default` row size (`HistoryViewController.swift:110`), and the
  reusable cell's 11 pt default font is reset the same way. In the user's screenshot of the
  History tab the hashes draw in the proportional system font.
- `HistoryViewController.swift:818` sets the same monospaced font on the changed-files rows of
  the commit drawer.
- `mac/Sources/RcGitApp/BlamePanelController.swift:108` and `:112` set monospaced fonts on the
  blame panel's line and hash columns, in a `.small` table.

## Prevention

- Never style a plain-string `textField` outlet per row in a table whose `rowSizeStyle` is not
  `.custom`. A font, weight, or size set on it in `viewFor` lasts until the next layout. Keep
  the label off the outlet and style it in the cell, give it an `NSAttributedString` whose runs
  carry the font, or set the table's `rowSizeStyle` to `.custom` when it does not need the
  system's row sizes.
- When a label leaves the outlet, implement `outlineView(_:typeSelectStringFor:item:)` (or the
  table view's equivalent) in the same change. The default reads the outlet, and with the
  outlet nil it has no string for those rows.
- Every icon-and-label sidebar row is a `SidebarCellView` (`.claude/rules/swift.md:76`). Add a
  new per-row style as a cell property whose `didSet` calls `applyRowSize()`, not as a font
  the controller writes.
- A value read back right after setting it proves nothing about a table cell. Read it after
  `window.displayIfNeeded()` and `layoutSubtreeIfNeeded()` in an offscreen window, with the
  harness above. Reading in `viewFor` or `didAdd` reports the value AppKit is about to
  replace.
- The width fit rests on measured source-list geometry (the 16 pt inset, the text-width
  alignment rect). Re-measure with the same harness after an SDK update rather than trusting
  the constants.

## Related Issues

- `docs/solutions/ui-bugs/attributed-string-label-ignores-cell-linebreakmode.md` — the same
  split from the other side: field-level text settings (line break mode there, font here)
  govern a plain string, and an attributed string brings its own.
- `docs/solutions/ui-bugs/nsstackview-hidden-at-attach-never-gets-width-alignment.md` —
  another silent AppKit layout behavior in the same sidebar, found by an offscreen probe
  rather than a launch.
- `docs/solutions/best-practices/shifted-punctuation-key-equivalent-cgevent-probe.md` — the
  same headless `xcrun swiftc` probe approach, applied to key equivalents.
- `.claude/rules/swift.md:76` — the standing rule for sidebar lists and `SidebarCellView`.
