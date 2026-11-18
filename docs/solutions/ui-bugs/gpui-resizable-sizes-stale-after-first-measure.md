---
title: "A gpui-component split's sizes() stops following layout after a panel is first measured"
date: 2026-11-18
category: ui-bugs
module: rc-git-app
problem_type: test_failure
component: frontend
symptoms:
  - "A test reading `ResizableState::sizes()[0]` for an unsized panel gets a height that no longer matches what is drawn"
  - "The History detail share test computed the commit list from `sizes()[0]` and the detail's share came out wrong, though the detail drew at 70%"
root_cause: api_misuse
resolution_type: test_fix
severity: low
framework_version: "gpui-component 0.9.0 (gpui-base 0.9.0)"
related_components: [gpui-component-resizable, resizable-state, history-splits]
tags: [gpui-component, gpui-base, resizable, resizable-state, sizes, container-size, split, panel-min-size]
---

# A gpui-component split's sizes() stops following layout after a panel is first measured

## Problem

`ResizableState::sizes()` reads like the size each panel of a split is drawn at. It is not.
An entry is copied from layout only while it still holds the placeholder `PANEL_MIN_SIZE`
(100px). Once a panel has been measured, its entry keeps that value until a drag,
`resize_panel`, a change in the number of panels, or a change in the container's own size
rewrites it. A sibling panel appearing from `.visible(false)`, or being given a size, leaves
it alone. So code or tests that read `sizes()` to learn what a split shows can read a
stale number.

## Symptoms

- In `an_unsized_commit_detail_opens_at_its_share_of_the_pane`
  (`crates/rc-git-app/src/tests/history.rs`), the first version took the commit list's
  height from `sizes()[0]` and derived the detail's share from it. The share came out wrong
  while the detail drew at its 70% share.
- Nothing errors: the stale entry is a plausible pixel value.

## What Didn't Work

- Treating `sizes()[0]` as the commit list's drawn height. The commit list is the split's
  first panel and has no `.size(..)`. It is measured while the detail panel is still hidden,
  so its entry records the whole pane's height. When the commit opens and the detail panel
  draws at its share, the container's size has not changed, so nothing rewrites the list's
  entry, and the two entries sum to more than the pane.

## Solution

Measure against the container, and read `sizes()` only for a panel whose entry the app set
itself. The test now divides the detail's entry by `container_size()`:

```rust
let (container, detail) = visual.read(|cx| {
    let state = column.read(cx);
    (f32::from(state.container_size()), f32::from(state.sizes()[1]))
});
let opened = detail / container;
```

The detail panel is opened with `.size(detail_height)` (`render_splits` in
`crates/rc-git-app/src/history/mod.rs`), so its entry holds the size the app asked for.
`container_size()` is the group's own laid-out bounds along its axis (gpui-base
`src/resizable/mod.rs:273`), so it is always current.

App code already sizes this way: `Split::available` in
`crates/rc-git-app/src/history/splits.rs` measures `container_size()` and falls back to the
viewport before the first layout.

## Why This Works

In gpui-base 0.9.0 `update_panel_size` runs as a panel prepaints, and it writes `sizes[ix]`
only behind `if self.sizes[panel_ix].as_f32() == PANEL_MIN_SIZE.as_f32()`
(`src/resizable/mod.rs:235`). The comment above that check says it exists "to stop the very
first panel from resizing on its own". Every later frame updates `panels[ix].bounds` and
leaves `sizes[ix]` alone. A panel built with `.visible(false)` renders an empty div and
reports no bounds at all (`src/resizable/panel.rs:322`), so its appearance later is not a
resize of its siblings. The entries are rescaled only when the group's own bounds change
along its axis (`src/resizable/panel.rs:181-187`, calling `adjust_to_container_size`), and
that rescale skips the split entirely while any panel has not been measured yet
(`panel.size` is `None`, `src/resizable/mod.rs:396`; the first measure sets it, `:237`).

After a drag, `sizes` and every panel's `size` come from the same vector
(`src/resizable/mod.rs:374-377`), and the panel's `size` drives its flex basis, so `sizes()`
read in a `ResizablePanelEvent::Resized` handler, as `Split::resized` does, matches what is
drawn. The drag does not first re-measure the layout: `resize_panel_at_handle` clones
`sizes` at `:309`, and the `sync_real_panel_sizes` call at `:317` is overwritten when the
cloned vector is stored at `:377`. So the first drag computes from whatever stale entry was
there.

## Prevention

- To learn what a split draws, use `container_size()` together with a panel you sized with
  `.size(..)`, or a `debug_bounds` selector on the panel's content. Never use the `sizes()`
  entry of a panel with no `.size(..)`.
- Reading `sizes()` right after a `Resized` event is fine, since the drag wrote every entry.
- A lint could flag `sizes()` reads outside resize handlers through `clippy.toml`
  `disallowed-methods`, but it would need `#[expect]` on the legitimate reads in
  `splits.rs` and the tests, a product-code change, so it is not in place.

## Related Issues

- `docs/solutions/ui-bugs/gpui-rounded-card-clips-children-square.md`: the other
  gpui-component resizable gotcha, where a handle renderer returning `None` keeps the divider.
