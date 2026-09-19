---
title: "tree-sitter-highlight's injection layer merge misorders and drops events"
date: 2026-09-19
category: integration-issues
module: rc-git-highlight
problem_type: integration_issue
component: infrastructure
symptoms:
  - "At equal byte offsets, a child injection layer's HighlightStart arrives before the parent layer's Start covering the same region, so an innermost-wins stack flattener mis-attributes runs (the markdown source around a fenced block rendered as Keyword)"
  - "Events from the injected markdown_inline layer are missing entirely: no emphasis or strong spans anywhere in the output"
  - "Each grammar highlights correctly on its own; only the merged multi-layer stream from Highlighter::highlight with an injection callback is wrong"
root_cause: wrong_api
resolution_type: code_fix
severity: medium
framework_version: "tree-sitter-highlight 0.27.0"
related_components: [rc-git-ffi, RcGitKit]
tags:
  [
    tree-sitter,
    tree-sitter-highlight,
    tree-sitter-md,
    language-injection,
    markdown,
    highlight-events,
    span-merging,
    interval-subtraction,
  ]
---

# tree-sitter-highlight's injection layer merge misorders and drops events

## Problem

Adding markdown to `rc-git-highlight` (pinned `tree-sitter =0.27.0`, `tree-sitter-highlight
=0.27.0`, `tree-sitter-md =0.5.3` with `default-features = false`) requires injections:
markdown's grammar is split into a block grammar plus an inline grammar reached via a
`"markdown_inline"` injection, and fenced code blocks inject their named language. Wiring
those injections through tree-sitter-highlight's own injection callback produced wrong or
missing highlights.

## Symptoms

The first attempt passed an injection callback to the library's multi-layer path —
`highlighter.highlight(config, data, None, None, |name| injection_configuration(name))` —
with every `HighlightConfiguration` configured with the same capture-name table. Dumping the
raw `HighlightEvent` stream showed two distinct failures:

1. **Mis-ordered Start/End events at equal byte offsets.** For a document holding a heading
   and a ` ```rust ` fence around `fn fenced() -> u32 { 1 }`, the merged stream arrived as:
   `START keyword` (injected rust layer), `START none` (parent block layer's
   `code_fence_content`), `START punctuation`, `END`, `Source "fn"`, `END`,
   `Source " fenced() -> u32 { "`. The injected layer's `Start(keyword)` came *before* the
   parent layer's `Start(none)`, plus an empty Start/End punctuation pair, so an
   innermost-wins stack flattener attributed `"fn"` to `none` (dropped) and
   `" fenced() -> u32 { "` to `Keyword`.
2. **The inline layer's events never appeared.** `"Some *soft* text."` produced zero emphasis
   or delimiter events, even though the injection callback was demonstrably invoked with
   `"markdown_inline"` and returned a valid config.

## What Didn't Work

The injection-callback approach itself. This was not a configuration bug: the callback was
invoked with the expected injection names, the returned `HighlightConfiguration`s were valid
and shared the same capture-name table, and each grammar highlighted correctly on its own.
The fault is in the library's cross-layer event merge — at equal offsets it interleaves
`HighlightStart`/`HighlightEnd` from different layers in the wrong nesting order, and it can
drop an injected layer's events entirely. No amount of consumer-side stack discipline
recovers correct attribution from a stream whose nesting order is wrong at the source.

## Solution

The fix, in `crates/rc-git-highlight/src/lib.rs`, stops using tree-sitter-highlight's
layer merge entirely and resolves injections by hand.

- `single_layer()` runs exactly one grammar with `|_| None` — no injections — and flattens
  the events innermost-wins into non-overlapping spans sorted by start.
- `highlight_markdown()` is a `single_layer` pass with the block grammar for the base, plus
  `markdown_overlays()`, merged by `merge_overlays()`.
- `markdown_overlays()` parses the document *again* with `tree_sitter::Parser` and the block
  grammar, then walks the tree with a `TreeCursor` looking for injection sites by node kind:
  `"inline"` → the inline grammar; `"fenced_code_block"` → read the `info_string > language`
  child, map it via `Language::from_injection`, take the `code_fence_content` range
  (`append_fence`); `"minus_metadata"` → YAML; `"plus_metadata"` → TOML. Each region's byte
  *slice* is highlighted recursively and its spans shifted back to document offsets
  (`append_region`). Markdown-in-markdown fences recurse with a depth counter capped at
  `MARKDOWN_FENCE_DEPTH`; beyond it an inner fence stays plain.
- `merge_overlays()` clips base spans around every overlay span they touch — interval
  subtraction, overlays win — then merge-sorts the two lists, preserving the sorted,
  non-overlapping contract the FFI and the Swift side rely on.

Sketch of the shape:

```rust
fn highlight_markdown(data: &[u8], depth: u8) -> Result<Vec<Span>, Error> {
    let base = single_layer(block_config, data, Language::Markdown)?; // no injections
    let overlays = markdown_overlays(data, depth); // second parse + TreeCursor walk
    Ok(merge_overlays(base, overlays)) // clip base around overlays, merge-sort
}
```

The document is parsed twice — once inside tree-sitter-highlight for the base, once for the
injection walk. That cost is accepted: input is capped at 2 MiB upstream, and a wrong
highlight is worse than a second parse.

## Why This Works

Single-layer event streams from tree-sitter-highlight are well-ordered; only the merge of
events *across* layers is broken. Walking the block tree for injection sites gives
deterministic byte regions with no ordering ambiguity, and highlighting each region's slice
in isolation keeps every stream single-layer. Interval subtraction in `merge_overlays()`
then reimposes the innermost-wins invariant that the event merge failed to provide — an
overlay is by construction deeper than the base, so clipping base spans around overlays
yields exactly the attribution the flawed merge was supposed to deliver.

## Prevention

- `markdown_merged_spans_stay_ordered_and_disjoint` pins the output contract: spans stay
  sorted by start and pairwise disjoint across a document mixing headings, inline emphasis,
  YAML front matter, code spans, and a TOML fence.
- `markdown_code_fences_highlight_in_their_own_language` pins fence injection: a `rust`
  fence gets Keyword/Function spans, and an unknown fence language stays plain.
- `markdown_prose_is_classified` pins the inline layer: heading, emphasis, strong, code
  span, and link classes all appear.
- Rule going forward: do not trust the relative ordering of `HighlightStart`/`HighlightEnd`
  across layers when injections are in play. If injections are needed, run each layer
  separately and combine spans yourself; only single-layer streams are safe to flatten with
  a stack.

## Related Issues

- `docs/solutions/architecture-patterns/hand-rolled-git-push-over-gix-https.md` — same
  shape: an upstream crate lacks or mis-implements needed behavior and the fix is
  hand-rolling it in-repo.
- `docs/solutions/integration-issues/russh-config-ssh-alias-resolution-divergence.md` —
  same shape: an upstream Rust crate silently diverges from reference behavior and is
  worked around locally.
