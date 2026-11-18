---
description: GPUI app rules for window tabs, the tab strip, wrapping text rows and rounded containers
paths: crates/rc-git-app/**, crates/rc-git-mac/src/window_tabs.rs
---

# GPUI app

- GPUI hides the native tab bar and records a tab group only when a window opens. Every path
  that joins a window to a group calls `register_tab_group` after `TabHost::join`, and every
  title change also goes through `SystemWindowTabController::update_tab_title`. Otherwise the
  app-drawn strip and Next/Previous Tab miss the window.
  (docs/solutions/ui-bugs/gpui-tabbed-window-hides-native-tab-bar.md)
- A text row in a wrapping `list` needs `w_full()` on the row and `flex_1().min_w_0()` on the
  text cell, or the text shapes as one line and the row clips; route the wrap switch through
  `list_rows::RowScroll`.
  (docs/solutions/ui-bugs/gpui-list-row-text-wraps-only-with-full-width-row.md)
- Write `list.active`, `table.active` and `selection` fills in `assets/theme.json` translucent:
  gpui-component clamps their alpha to 0.2/0.3, and drops a bad value or unknown key silently.
  (docs/solutions/ui-bugs/gpui-component-theme-config-silent-clamps-and-drops.md)
- Inside a rounded container, paint the background on the container only: GPUI clips
  `overflow_hidden` to a rectangle, so a child fill at its edge squares off the corners. Hide a
  split's divider with a handle renderer that returns an empty `Some`, never `None`.
  (docs/solutions/ui-bugs/gpui-rounded-card-clips-children-square.md)
