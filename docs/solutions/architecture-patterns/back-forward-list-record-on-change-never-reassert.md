---
title: "Back/forward stops are recorded only from a real change, never re-asserted"
date: 2026-09-30
category: architecture-patterns
module: rc-git-kit
problem_type: architecture_pattern
component: frontend
severity: medium
related_components:
  - rc-git-app
applies_when:
  - "Adding a new way to move in the app (a jump, a link, a picker, a pane switch) that Back should return from"
  - "A restore can miss its stop because the file was deleted or the ref is gone"
  - "Tempted to record the current location from a lifecycle event or every model event, to be safe"
  - "Back or Forward can be pressed again while the previous restore is still loading"
  - "Building any history whose current entry can disagree with what is on screen"
tags:
  - back-forward-list
  - navigation-history
  - navigation-stop
  - browser-model
  - failed-restore
  - forward-stops
  - coalescing
  - generation-counter
---

# Back/forward stops are recorded only from a real change, never re-asserted

## Context

The back/forward feature keeps one `BackForwardList` for the whole app, held by the one
`Navigator` (`crates/rc-git-kit/src/navigator.rs`) that the app's `Shell` owns. A stop is a
tab's repository path, its pane, and a `BrowserLocation`. The location names a tree, a file
path, a history scope and a detail commit, and holds none of them (`BrowsedTree` and
`BrowserLocation` in `crates/rc-git-kit/src/back_forward.rs`). `BrowserModel::restore`
(`crates/rc-git-kit/src/browser/location.rs`) reads what the stop names from the repository as
it is at that moment.

So a restore can end somewhere other than its stop. A deleted ref reports an error and leaves
the browser where it was (the `BrowsedTree::Ref` case of `restore_tree`). A file that is no
longer in the tree does not open (`open_path` answers other than `OpenPathOutcome::Opened`),
and `restore_file` then empties the pane with `close_file`, so the file of the stop left
behind does not stay open under another stop's name. From then on the list's current stop and
the screen disagree until the user moves.

`BackForwardList::visit` ignores only a stop equal to the current one. Any other stop drops the
stops ahead and is appended. While list and screen disagree, a call that records "where the
tab is" counts as a move to a new place, and Forward is gone with the user having done
nothing.

## Guidance

Record a stop only when something changed. Do not record to re-assert where the tab already
is. The tree has three sources of a record:

- **The model's location changed.** `note_location` compares `location()` with
  `reported_location` and emits `BrowserEvent::LocationChanged` only on a difference, and only
  while no move is under way. A method whose change spans an await brackets itself in
  `begin_move`/`end_move`, so it reports once, where it ended. The window (`RepoWindow`,
  `crates/rc-git-app/src/window/mod.rs`) turns the event into `record_stop`, which emits
  `WindowEvent::Stop`.
- **The user acted on the pane.** `switch_mode` is `set_mode` plus `record_stop`, for a pane
  picked by itself. An action that changes the pane on its way somewhere calls `set_mode`,
  then the model, then records when the model's task returns (`record_stop_after`). When the
  model already reported the move, that last record equals the current stop and `visit` drops
  it. It covers the case where the pane changed and the location did not.
- **Another tab came to the front.** `Navigator::tab_came_to_front` returns when the tab is
  already `front`, which is what the same window becoming main on app reactivation looks like.
  `Shell::go` calls `Navigator::begin_restore`, which sets `front`, before it shows the stop's
  window, so the tab switch a step makes is not recorded either.

`Navigator::report` then filters what arrives: nothing while `pending_restores` is non-zero,
and nothing from a tab that is not in front. `Shell` only forwards window events to it
(`WindowEvent::Stop` and `WindowEvent::BecameFront` in `Shell::watch`). A restore that moved
the browser still ends with a model report, because `restore` calls `end_move` when its loop
ends. The app drops that report, and `reported_location` now matches the screen, so a later
ref refresh that moves nothing has no difference to report.

## Why This Matters

Four other designs were considered while building the feature and rejected.

1. **Back returns to the list's current stop when the screen is not on it.** This would fix the
   empty-tab gap below. A stop that cannot be restored never matches the screen, though: the
   deleted-ref restore changes nothing (`restore_tree`). Each Back press would retry the same
   stop and the user could not get past it.
2. **Rewrite the current stop with where the restore ended.** The rewritten stop can equal its
   neighbor, and then one Back press changes nothing. `remove_stops` merges equal neighbors for
   the same reason. The list has no call that rewrites a stop; `stops` is private.
3. **Record from every model event.** `select` sets the selected ref before its await, and the
   root load closes the open file after it. A record in between is the new ref with the old
   file, a stop nobody was at.
4. **Let restores overlap and drop the stale one with a generation check after each await.**
   `location().tree` comes from the selected ref, which `select` sets synchronously. A second
   restore into the same tree would find the tree already matching, skip the tree step, and
   resolve its file path in `open_path` against the root tree the first restore has not
   replaced yet. Restores coalesce instead, with `is_restoring` and `pending_restore` in
   `restore`, the in-flight-plus-pending shape `ChangesModel` (`crates/rc-git-kit/src/changes/`)
   uses for its scans.

Accepted gap: a tab with no repository has no stop, since `RepoWindow::current_stop` is `None`.
Coming to the front records nothing, and the list's mark stays on the tab the user left. Back
pressed in the empty tab goes to the stop before that one.

## When to Apply

- Adding a `BrowserModel` method that changes the tree, the open file, the history scope or the
  detail commit. Call `note_location` after the change, or bracket the method in
  `begin_move`/`end_move` when the change spans an await. Do not emit `LocationChanged`
  directly.
- Adding a UI action that changes the pane. Use `set_mode`, the model call, then record. Use
  `switch_mode` only when the pane is the whole action.
- Wanting to record or `visit` from a lifecycle event to keep the list in step with the screen:
  the window became main, refs refreshed, a restore finished, a repository attached. The rule
  excludes those calls.
- Reviewing a change to `Shell` or `RepoWindow` near the list. The recording rules live in
  `Navigator` so its tests can pin them; the app side is checked by reading its forwarding
  against the three sources above.
- Building any history whose entries name things and re-read them on return.

## Examples

The model reports a difference and nothing else (`BrowserModel::note_location`):

```rust
pub(super) fn note_location(&mut self, cx: &mut Context<Self>) {
    if self.move_depth > 0 {
        return;
    }
    let current = self.location();
    if current == self.reported_location {
        return;
    }
    self.reported_location = current;
    Self::changed(BrowserEvent::LocationChanged, cx);
}
```

The same window becoming main again is not a move (`Navigator::tab_came_to_front`):

```rust
pub fn tab_came_to_front(&mut self, tab: &Tab, stop: Option<NavigationStop>) -> bool {
    if self.front.as_ref() == Some(tab) {
        return false;
    }
    self.front = Some(tab.clone());
    let Some(stop) = stop else {
        return false;
    };
    self.report(stop, tab)
}
```

What the `front` guard prevents: the list is `[A, B, C]` with the mark on `B` after one step
back, and `B`'s file is gone, so the screen shows `B`'s ref with no file. The user switches to
another app and back. A window-became-main handler that recorded `current_stop` would visit a
stop that differs from `B`, and `visit` would drop `C`.

Tests that pin the behavior, in `crates/rc-git-kit/tests/`:

- `every_move_reports_once` (`browser/location.rs`): the same file opened again reports
  nothing, and neither does a refresh that moved nothing.
- `a_ref_switch_reports_where_it_ended_and_not_the_step_between` (`browser/location.rs`): one
  report for a ref switch, without the old file.
- `restore_reports_where_it_ended_once` and `two_quick_restores_into_another_tree_end_on_the_second`
  (`browser/location.rs`): one report for a restore and one for a coalesced pair, and the
  second of two overlapping restores opens its file against the tree the first one loaded.
- `restore_of_a_file_gone_from_the_same_tree_closes_the_one_open` (`browser/location.rs`): a
  restore that falls short leaves an empty pane, which is the state the list then disagrees
  with.
- `the_same_tab_becoming_main_again_records_nothing`, `a_background_tabs_report_is_ignored`
  and `a_report_during_a_restore_is_ignored_and_forward_survives` (`navigator.rs`): the three
  filters `Navigator` applies before a report reaches the list.
- `visiting_the_current_stop_records_nothing` and
  `a_visit_after_stepping_back_drops_the_stops_ahead` (`back_forward.rs`): the two halves of
  `visit` that make a record without a change cost the stops ahead.

## Related

- [A Task per filesystem event](../runtime-errors/task-per-watch-event-backlog.md): where the
  in-flight flag plus one pending slot comes from. There it bounds work a watcher asks for; here
  it orders two restores, and the pending slot carries the newest location.
- `docs/architecture.md` ("Going back") has the flow.
- `CONCEPTS.md` defines a stop.
