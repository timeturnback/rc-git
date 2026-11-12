---
title: "A tabbed GPUI window shows no tab bar: GPUI hides AppKit's and tracks groups only at open"
date: 2026-11-12
category: ui-bugs
module: rc-git-app
problem_type: ui_bug
component: frontend
symptoms:
  - "Open Repository (Cmd+O) opens the repository, but no tab bar appears and the previous repository seems gone"
  - "CGWindowList shows several RCGit windows with the same frame and only one on screen: they are native tabs of one group"
  - "Ctrl+Tab (Next Tab) does nothing after a window was joined to a group with addTabbedWindow"
root_cause: api_misuse
resolution_type: code_fix
severity: high
framework_version: "gpui-pre 0.3.11, gpui-component 0.9.0"
related_components: [gpui, appkit, nswindow-tabbing, system-window-tab-controller]
tags:
  [
    gpui,
    native-tabs,
    tab-bar,
    addtabbedwindow,
    systemwindowtabcontroller,
    titlebar-accessory,
    observe-global,
  ]
---

# A tabbed GPUI window shows no tab bar: GPUI hides AppKit's and tracks groups only at open

## Problem

After the GPUI app learned to join each new repository window to the front window's tab
group (`TabHost::join` in `crates/rc-git-mac/src/window_tabs.rs`, through
`-[NSWindow addTabbedWindow:ordered:]`), Open Repository still looked like it replaced the
window: the join worked, but no tab bar was drawn, so the other tabs were invisible.

## Symptoms

- Cmd+O opens the repository with no tab bar; the earlier repository is still open, hidden.
- A `CGWindowListCopyWindowInfo` dump of the process shows the repository windows sharing one
  frame with only one on screen, which is what a native tab group looks like.
- Next/Previous Tab do nothing, because they walk GPUI's own record of the group.

## What Didn't Work

- Assuming AppKit draws the tab bar once windows share a group. That held for the AppKit app,
  whose tabs came from `newWindow()` plus `addTabbedWindow` and drew natively (session
  history), and it is why the earlier fix stopped at the join.
- Reading stderr for clues: the app logs only to stderr, so a Dock-launched build leaves
  nothing to read. The window list from CoreGraphics was what showed the tabs existed.

## Solution

GPUI owns the tab bar on macOS, so the app draws it:

1. `crates/rc-git-app/src/window/tab_bar.rs` renders a strip under the toolbar from
   `SystemWindowTabController::tabs(window_id)`: one equal-width tab per window (`flex_1()`
   on each gpui-component `Tab`), a close button per tab, and a plus that dispatches
   `OpenRepository`. It shows only while the group holds two or more tabs.
2. `register_tab_group` (`crates/rc-git-app/src/app.rs:893`) runs right after `TabHost::join`:
   `remove_tab` the new window, then `add_tab` it with `window.tabbed_windows()`, so GPUI's
   record matches the native group.
3. `RepoWindow::show` calls `SystemWindowTabController::update_tab_title` next to
   `set_window_title` (`crates/rc-git-app/src/window/mod.rs:295`), since setting the window
   title does not touch the controller.
4. Each window observes the controller global but redraws only when its group's
   `(window, title)` list changes (`shown_tabs`, `crates/rc-git-app/src/window/tab_bar.rs:24`;
   the observer at `crates/rc-git-app/src/window/mod.rs:534`).

## Why This Works

Paths starting `gpui-pre-` are inside those crates' sources in the cargo registry
(`~/.cargo/registry/src/index.crates.io-*/`), at the versions this repo pins.

- gpui-pre-macos overrides `addTitlebarAccessoryViewController:` on its window class and hides
  the accessory AppKit adds for the tab bar, setting its height to 0, with the comment "since
  we render our own" (`gpui-pre-macos-0.3.11/src/window.rs:4025`). No AppKit setting brings it
  back; GPUI expects the app to draw tabs from `SystemWindowTabController`, as Zed does.
- GPUI fills that controller only while a window opens: `Window::new` reads
  `platform_window.tabbed_windows()` and calls `add_tab` (`gpui-pre-0.3.11/src/window.rs:1700`).
  A window the app joins to a group afterwards is unknown to it until re-recorded, which is
  also why Next/Previous Tab were dead.
- `update_last_active` runs on every activation change (`gpui-pre-0.3.11/src/window.rs:2194`)
  and goes through `global_mut` (`gpui-pre-0.3.11/src/app.rs:682`, `:2562`), which notifies
  every `observe_global` observer. A plain `cx.notify()` observer would re-render every
  repository window on each focus switch; comparing the shown list keeps it to real changes.

## Prevention

- In the GPUI app, any native window-tabbing change needs three things together: the native
  join, `register_tab_group`, and a title update through the controller. A second path that
  opens or joins windows has to call `register_tab_group` too.
- `crates/rc-git-app/src/tests/window_tabs.rs` pins the strip: it appears only for two or
  more tabs, redraws when the group changes without a forced refresh, keeps tabs equal width
  across the full strip, selects and closes through the tab clicks, names a tab after the
  repository its window opens, and the plus dispatches `OpenRepository`. The test platform has no native groups, so the tests build the group in
  the controller directly; the native join itself is covered only by running the app.
- Known gap: `SystemWindowTabController::add_tab` keys a new group at `tab_groups.len()`
  (`gpui-pre-0.3.11/src/app.rs:759`), which can overwrite a live group after groups were
  removed. Treat a vanished group after Move Tab to New Window plus closes as this, not a new
  bug.
