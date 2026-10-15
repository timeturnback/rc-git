---
title: "A standalone NSTextBlock with no content width lays out one glyph per line"
date: 2026-09-28
category: ui-bugs
module: RcGitApp
problem_type: ui_bug
component: frontend
root_cause: api_misuse
resolution_type: code_fix
severity: high
framework_version: macos 26.6.2
related_components: [RcGitKit]
tags:
  [
    nstextblock,
    nstexttable,
    textkit,
    paragraph-style,
    markdown-preview,
    layout,
  ]
---

# A standalone NSTextBlock with no content width lays out one glyph per line

## Problem

The markdown preview draws code panels, blockquote bars, and thematic rules by putting an
`NSTextBlock` in the paragraph style's `textBlocks`. Tables use `NSTextTableBlock` the same
way. Tables laid out correctly and everything else collapsed: a fenced code block rendered
as a vertical column of single characters, one per line, down the whole pane.

## Symptoms

A `NSLayoutManager` measurement reproduces it without launching the app — lay the rendered
string out in a 600-point `NSTextContainer` and print `lineFragmentRect` per fragment:

```
w= 149.1 x=   0.0  Getting Started\n
w=  16.0 x=   8.0  # 
w=  16.0 x=   8.0  C
w=  16.0 x=   8.0  l
w=  16.0 x=   8.0  o
...
w=  97.9 x=   7.0  Package Mgr\n      <- NSTextTableBlock, correct
w=  62.1 x= 299.0  Bun 1.3\n
w= 588.5 x=   0.0  plain paragraph ...  <- no textBlocks, correct
```

Every fragment inside a bare `NSTextBlock` is 16 points wide — one monospaced glyph. Every
fragment inside an `NSTextTableBlock`, and every fragment in a paragraph with no `textBlocks`
at all, uses the container's width as expected. The blockquote bar and the thematic rule hit
the same thing; they are just less obvious, because a quote of short lines and a rule holding
one newline both look plausible while collapsed.

## What Didn't Work

Nothing about the container. `configureForPreview()` already sets
`widthTracksTextView = true` and sizes the container to the scroll view, and the table in the
same document proved the container was 600 points wide at the same moment the code block was
16. The width the text got was decided per block, not per container.

## Solution

Give every standalone block an explicit content width. In
`mac/Sources/RcGitApp/MarkdownRenderer.swift`, one factory now builds all of them:

```swift
static func fullWidthBlock() -> NSTextBlock {
    let block = NSTextBlock()
    block.setContentWidth(100, type: .percentageValueType)
    return block
}
```

`quoteBlock()`, `panel(background:)`, and the thematic rule's block are built from it. A
direct measurement of the four candidate fixes:

| block | first fragment width | fragments |
|---|---:|---|
| bare `NSTextBlock` | 16.0 | 51 |
| `setContentWidth(100, .percentageValueType)` | 475.9 | 1 |
| `setValue(100, .percentageValueType, for: .width)` | 475.9 | 1 |
| `setContentWidth(500, .absoluteValueType)` | 475.9 | 1 |
| one-column `NSTextTable` | 475.9 | 1 |

The percentage content width is the one to use: it is the only option of the four that is
both standalone and resolution-independent, so a nested quote takes 100% of its parent block
and the panel follows the pane as the window resizes.

## Why This Works

`NSTextBlock` has no intrinsic width. Inside an `NSTextTable` the table's layout algorithm
assigns each cell one, which is why `NSTextTableBlock` never shows the bug and why the
documented examples — all of them tables — never mention it. A block used on its own has
nobody to assign it a width, and TextKit falls back to the minimum that fits a glyph. The
API to say otherwise is `setContentWidth`, and a percentage resolves against the enclosing
block or the container, so it stays correct under nesting and resizing.

## Prevention

- Measure layout instead of launching the app: build `NSTextStorage` + `NSLayoutManager` +
  `NSTextContainer` at a fixed width, `ensureLayout(for:)`, and walk
  `lineFragmentRect(forGlyphAt:effectiveRange:)`. A fragment far narrower than the
  container for text that should wrap is the signature of this bug, and the same harness
  checks a narrow container in the same run.
- Rule going forward: any `NSTextBlock` that is not an `NSTextTableBlock` gets a content
  width at construction. Build them through one factory so a new block kind cannot be added
  without it.
- Both are enforced: `MarkdownRendererTests.standaloneBlocksFillThePane`
  (`mac/Tests/RcGitAppTests`) fails when any block's `lineFragmentRect` is 100 points or
  narrower in a 600-point container, and the "preview text blocks" step of `scripts/check.sh`
  fails when `mac/Sources` holds any `NSTextBlock()` besides the one in `fullWidthBlock()`.

## Related Issues

- `docs/solutions/integration-issues/foundation-markdown-parser-cannot-back-a-preview.md` —
  the change that introduced these blocks.
