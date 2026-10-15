---
title: "Foundation's markdown parser cannot back a document preview"
date: 2026-09-27
category: integration-issues
module: RcGitKit
problem_type: integration_issue
component: frontend
root_cause: wrong_api
resolution_type: code_fix
severity: medium
framework_version: "macos 26.6.2, swift 6"
related_components: [RcGitApp, rc-git-highlight]
tags:
  [
    markdown,
    attributed-string,
    presentation-intent,
    nstexttable,
    textkit,
    parser,
    performance,
    gfm,
  ]
---

# Foundation's markdown parser cannot back a document preview

## Problem

The file pane's markdown preview was built on `AttributedString(markdown:options:)` with
`interpretedSyntax = .full`, walking `parsed.runs` and mapping each run's
`presentationIntent` onto fonts and indents. Tables never appeared, and the parse dominated
the time to first preview. Fixing the tables alone would have left the other gaps in place,
so the question was whether Foundation's parser can back a preview at all.

## Symptoms

Probing the parser directly — build an `AttributedString` from a sample and print every
run's text, `presentationIntent.components`, `inlinePresentationIntent`, and `link` — is
what settles this, and it settles it in one file. Against a sample holding a table, task
items, an image, a footnote, an autolink, and strikethrough:

1. **Tables arrive as four bare cell runs.** The intents are there and correct —
   `tableCell 0`, `tableHeaderRow`, `table [TableColumn(alignment: .left),
   TableColumn(alignment: .right)]` — but a renderer that walks runs and appends text puts
   `a`, `b`, `1`, `2` on consecutive lines with no geometry. Nothing is lost; nothing is
   laid out either.
2. **An image loses its destination entirely.** `![img](i.png)` produces one run whose text
   is `img`, whose `link` is `nil`, and which carries no intent marking it as an image. The
   source path is not recoverable from the parse.
3. **GFM task items are not parsed.** `- [ ] task` arrives as a list item whose text is the
   literal `[ ] task`.
4. **Footnotes are not parsed.** `[^1]` stays literal in the prose, and the definition line
   `[^1]: note text` comes through as an ordinary paragraph.
5. **The parse costs about 470 ms per MiB.** 1 115 886 bytes of this repository's own `.md`
   files took 467 ms in a release build on Apple silicon. Walking the resulting runs and
   building the `NSAttributedString` from them took 23 ms and 51 ms — the parse is
   essentially the whole cost, and it is what the preview limit was sized around.

Strikethrough, code spans, autolinks, and inline links do come through correctly. The
failure is not that the parser is broken; it is that its coverage stops short of GFM and its
throughput is an order of magnitude off what a file browser wants.

## What Didn't Work

Staying on Foundation and rendering the table intents. This was viable for tables in
isolation: the column alignments are in `PresentationIntent.TableColumn` and the cell
positions are in the `tableCell`/`tableRow` components, which is enough to build an
`NSTextTable`. It was rejected because it fixes exactly one of the five findings above. The
image destination cannot be recovered at all, task items and footnotes would need the source
re-scanned alongside the parse, and none of it touches the 470 ms per MiB.

## Solution

`MarkdownDocument.parse` in `mac/Sources/RcGitKit/Markdown.swift` replaces the Foundation
parse. It scans `Array(source.utf8)` and produces a flat `[MarkdownBlock]`, each block
holding `[MarkdownRun]` plus a `quoteDepth` and a `listDepth`, so nesting needs no tree.
`MarkdownRenderer` (RcGitApp) maps that model onto the attributed string and nothing else;
it never re-reads the source.

Two properties carry the design:

- **Byte scanning is safe here.** Every delimiter markdown uses is ASCII, and in UTF-8 a
  continuation byte is always `>= 0x80`, so a byte at `0x2A` is a literal `*` and never part
  of a multi-byte character. This is what removes the grapheme-cluster walk that
  `AttributedString`'s character view forces.
- **Flat blocks, not a tree.** `MarkdownBlockParser` keeps a stack of list *content indents*
  (`listIndents`) and pops it against the line's indent. A block's `listDepth` is that
  stack's depth at the time it was emitted, so a renderer indents from two integers instead
  of recursing.

On the same 1 115 886 bytes the new parser takes 20 ms, against Foundation's 467 ms, and it
finds the 54 tables in those files that the old path rendered as loose text. Coverage now
includes both heading forms, fenced and indented code, blockquotes, nested lists, GFM task
items and tables, YAML front matter, footnotes, reference links, images, bare-URL
autolinks, and raw HTML blocks.

## Why This Works

`AttributedString(markdown:)` is built to produce *display text with intent annotations* for
a label, not a document model for a renderer. That is why an image collapses to its alt text
— a label has nowhere to put a source URL — and why the extensions a document browser wants
are the ones missing. The cost follows from the same shape: it materialises an
`AttributedString` over the whole document, with its `Character`-keyed indices and per-run
attribute containers, where the preview only ever needed offsets and enum cases.

Owning the parse also moves it into `RcGitKit`, where it is pure and reachable by
`swift test`. The Foundation path had its behaviour only in `RcGitApp`, which has no tests,
so every one of the five findings above had to be discovered by probing a framework rather
than by reading a failing assertion.

## Prevention

- `mac/Tests/RcGitKitTests/MarkdownTests.swift` pins each syntax the preview claims: a
  table's alignments and its padding of short rows, task items, nested list depth, setext
  headings, front matter, footnote split, reference links resolved in either order, and the
  inline cases that must *not* fire (`snake_case_name`, `2 * 3 = 6`, `**never closed`).
- `docs/performance.md` carries the preview's budget in its "markdown preview" row — parse
  plus attributed-string build, off the main actor, capped at 8 MiB — so a change that
  regresses it has a number to fail against.
- Rule going forward: before adopting a platform parser for a format with extensions, write
  the ten-line probe that dumps its parse of a sample covering those extensions. Presentation
  intents existing for a construct does not mean the construct is usable, and a construct
  parsing does not mean its data survived. Both were true of the same parser at once.

## Related Issues

- `docs/solutions/integration-issues/tree-sitter-highlight-injection-merge-misordered-missing-events.md`
  — same shape: an upstream highlighting API is adopted, found to lose or mis-order what the
  consumer needs, and replaced with an in-repo walk. That doc's `Language::from_fence` is what
  the new preview calls to color a fenced block.
- `docs/solutions/architecture-patterns/hand-rolled-git-push-over-gix-https.md` — same shape:
  an upstream crate does not cover the needed behaviour and the fix is hand-rolling it.
