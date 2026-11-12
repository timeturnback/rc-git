---
description: GPUI app rules for window tabs and the tab strip
paths: crates/rc-git-app/**, crates/rc-git-mac/src/window_tabs.rs
---

# GPUI app

- GPUI hides the native tab bar and records a tab group only when a window opens. Every path
  that joins a window to a group calls `register_tab_group` after `TabHost::join`, and every
  title change also goes through `SystemWindowTabController::update_tab_title`. Otherwise the
  app-drawn strip and Next/Previous Tab miss the window.
  (docs/solutions/ui-bugs/gpui-tabbed-window-hides-native-tab-bar.md)
