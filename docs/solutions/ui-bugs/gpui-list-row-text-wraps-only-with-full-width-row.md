---
title: "Text in a GPUI list row stays on one line unless the row is full width"
date: 2026-11-12
category: ui-bugs
module: rc-git-app
problem_type: ui_bug
component: frontend
symptoms:
  - "With Wrap Lines on, a long file line draws on one row and is clipped at the pane's edge instead of wrapping"
  - "The wrapping list measures a long row at one line high: the ink test fails with the long row wraps 19.5px over 19.5px"
root_cause: api_misuse
resolution_type: code_fix
severity: medium
framework_version: "gpui-pre 0.3.11, gpui-component 0.9.0"
related_components: [gpui, gpui-list, list-state, uniform-list, styled-text]
tags: [gpui, list, liststate, text-wrap, w-full, min-w-0, row-height, file-viewer, diff-view]
---

# Text in a GPUI list row stays on one line unless the row is full width

## Problem

The file viewer's wrap mode draws its rows in a variable-height GPUI `list`, the same way the
diff view does. The viewer's row was a flex `div` with the line-number gutter and a text cell,
but the row had no width set. In that state the text never wrapped: every row measured one line
high, and a long line ran past the pane's edge.

## Symptoms

- With View > Wrap Lines on, a file line wider than the pane is clipped, not wrapped.
- `wrapped_rows_grow_taller_and_only_the_rows_on_screen_are_shaped`
  (`crates/rc-git-app/src/tests/viewer_wrap_ink.rs:79`) failed with
  `the long row wraps: 19.5px over 19.5px`. The long row was as tall as a one-word row.

## What Didn't Work

- Giving only the text cell `flex_1().min_w_0()`. Those settings let the cell shrink to fit its
  row, but the row itself was not limited to the pane's width, so there was nothing narrower
  to shrink to.
- `w_full()` does not help everywhere. On gpui-component's `TabBar` it changed nothing, because
  that bar's parent already stretched it to the full width (session history). It matters only
  where the parent does not stretch the row, as with a `list` item laid out on its own.

## Solution

Give the row `w_full()` in wrap mode, and give the text cell `flex_1().min_w_0()`. In one-line
mode the row keeps `whitespace_nowrap()` instead, and the cell keeps `flex_none()`:

```rust
div()
    .id(index)
    .flex()
    .when(wraps, Styled::w_full)
    .when(!wraps, Styled::whitespace_nowrap)
    // gutter …
    .child(
        div()
            .when(wraps, |cell| cell.flex_1().min_w_0())
            .when(!wraps, Styled::flex_none)
            .child(text),
    )
```

See `crates/rc-git-app/src/files/viewer.rs:286` and `:304`. The diff view's row already did
this: `crates/rc-git-app/src/diff/mod.rs:392` sets `.w_full()`, and `body_cell` (`:535`) sets
`flex_1().min_w_0()` when wrapping, at `:538`. That is why the diff view wrapped and the
viewer did not.

## Why This Works

GPUI's `list` lays out each item on its own, as a root, at the list's width and a
content-driven height. It passes `AvailableSpace::Definite(width)` and
`AvailableSpace::MinContent` to `layout_as_root`
(`src/elements/list.rs:1069-1093` in the `gpui-pre` 0.3.11 crate source, under
`~/.cargo/registry/src/`). A row with no width of its own was laid
out at the width of its content, not the list's width; this session's test is the evidence,
and it agrees with how the diff row avoids the problem. A row as wide as its content gives the
text no narrower width to wrap at, so `StyledText` shapes it as one line. `w_full()` makes the
row the list's width. `min_w_0()` then lets the text cell shrink below its unwrapped width,
so the text wraps to the space left after the gutter.

## Prevention

- Any new view that draws text rows in a wrapping `list` needs both pieces: a full-width row
  and a text cell with `flex_1().min_w_0()`. Its long-line wrap and one-line switch should go
  through `RowScroll` and `WrapLines` in `crates/rc-git-app/src/list_rows.rs`, which the diff
  view and the file viewer share.
- Prove wrapping with a headless ink test over real CoreText, not by reading layout code. The
  two that exist compare a long row's height with a short row's:
  `crates/rc-git-app/src/tests/viewer_wrap_ink.rs:79` and
  `crates/rc-git-app/src/tests/diff_ink.rs:115`. Taking out `w_full()` made the viewer test
  fail when this fix was made. Check that before you trust such a test. A layout test in an
  earlier session passed with its fix removed, because its fixture never produced the shape
  the fix changes (session history). Here that means a line long enough to wrap at the test
  window's width.
- The views still build their rows separately, so this pair of settings is repeated in each.
  Sharing the row shell between the diff view and the file viewer would leave one place to get
  it right. That is a code change for the app's owner, not something this note can enforce.

## Related Issues

- `docs/solutions/ui-bugs/gpui-tabbed-window-hides-native-tab-bar.md`: another GPUI layout
  surprise in `rc-git-app`.
