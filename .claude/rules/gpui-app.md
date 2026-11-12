---
description: GPUI app rules for window tabs, the tab strip and wrapping text rows
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
