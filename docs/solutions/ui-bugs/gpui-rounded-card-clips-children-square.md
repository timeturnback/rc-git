---
title: "A rounded GPUI card clips its children square, and a split divider hides only through its renderer"
date: 2026-11-18
category: ui-bugs
module: rc-git-app
problem_type: ui_bug
component: frontend
symptoms:
  - "A child that paints its own background flush to a rounded card's edge shows square corners past the card's rounded ones"
  - "A resizable split's hairline still draws after a custom handle renderer returns None"
root_cause: api_misuse
resolution_type: code_fix
severity: low
framework_version: "gpui-pre 0.3.11, gpui-component 0.9.0"
related_components: [gpui, content-mask, overflow-hidden, gpui-component-resizable, resize-handle]
tags: [gpui, rounded, overflow-hidden, content-mask, card, resizable, divider, resize-handle, window-layout]
---

# A rounded GPUI card clips its children square, and a split divider hides only through its renderer

## Problem

The repository window puts the mode's pane in a rounded, outlined card set in from the window
chrome, beside the sidebar, with no line between the sidebar and the card. Two GPUI details
decide whether that holds. `overflow_hidden()` on a `.rounded()` element does not clip its
children to the curve. A gpui-component split draws its divider line unless a renderer
replaces it with something else.

## Symptoms

- A pane root with `.bg(theme.background)`, a notice bar with `.bg(theme.muted)`, or a
  full-width tinted diff row at the card's top or bottom paints a square corner over the
  chrome, past the card's rounded corner.
- After `with_handle_appearance` is given a renderer that returns `None`, the hairline between
  the sidebar and the card still draws.

## What Didn't Work

- Relying on `overflow_hidden()` to round off the children. GPUI's `ContentMask` holds only
  `bounds` (gpui-pre `src/window.rs:2455`), so the clip is a rectangle.
- Returning `None` from the handle renderer to mean "draw nothing". gpui-base documents that
  `None` keeps the built-in line, so a renderer can leave some handles alone
  (gpui-base `src/resizable/resize_handle.rs:52`).

## Solution

The card paints the page background once, and its children stay off its corners:

- The file pane and History pane roots no longer paint `theme.background`. The card already
  paints it.
- The restore notice, which sits at the card's top, rounds its top corners by the card's
  inner radius: `.rounded_t(style::RADIUS_LG - style::HAIRLINE)`
  (`crates/rc-git-app/src/window/mod.rs`, `render_notice`).

The divider is replaced, not removed (`quiet_divider` in `crates/rc-git-app/src/window/mod.rs`):

```rust
fn quiet_divider(split: ResizablePanelGroup) -> ResizablePanelGroup {
    let drawn = gpui_component::resize_handle_appearance();
    split.with_handle_appearance(Rc::new(move |handle, window, cx| {
        if handle.is_active() {
            return drawn(handle, window, cx);
        }
        Some(div().flex_none().w(style::HAIRLINE).h_full().into_any_element())
    }))
}
```

At rest it returns an empty element with the hairline's width, so the split's band, cursor and
drag stay as they were. While the handle is held it hands over to the component's own drawing,
the pill.

## Why This Works

A child's background is painted inside the card's border box, and only a rectangle clips it.
At a 10 px radius the corner region outside the curve is about 3 px deep, so any child fill
that reaches the edge shows there. Leaving the fill to the card, and rounding the one bar that
has to sit at the edge, keeps every corner pixel the card's.

`h_resizable` installs `resize_handle_appearance()`, and a second `with_handle_appearance`
call replaces it. The renderer has to return `Some` to suppress the line. The closure's
parameter types come from the expected `ResizeHandleRenderer` when the `Rc::new(...)` is passed
straight to `with_handle_appearance`, so the app needs no import from gpui-base.

## Prevention

- Inside a rounded container, paint the background on the container and nowhere below it. A
  bar that must reach the container's edge rounds its own outer corners by the container's
  radius minus its border.
- `ResizeHandleContext::is_active()` is `Pressed | Dragging` only
  (gpui-base `src/resizable/resize_handle.rs:117`), not `Hovered`. gpui-component does not
  re-export `ResizeHandleState`, so a renderer in the app cannot keep the hover pill without
  depending on gpui-base directly. The quiet divider shows the pill only while held.
- Still open: a full-width tinted diff row at the card's first or last line has the same
  square corner. An ink test that reads the card's corner pixels would catch every case. The
  app's ink harness (`crates/rc-git-app/src/tests/ink.rs`) can capture the window.

## Related Issues

- `docs/solutions/ui-bugs/gpui-component-theme-config-silent-clamps-and-drops.md`: other
  gpui-component drawing that lands on the app (TabBar's own bottom border).
- `docs/solutions/ui-bugs/gpui-tabbed-window-hides-native-tab-bar.md`: the app-drawn window
  tab strip this card sits under.
