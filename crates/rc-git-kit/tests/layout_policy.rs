//! Window layout thresholds, split pane sizing, and the sidebar's collapse and fit
//! bookkeeping. Port of `LayoutPolicyTests.swift`.
#![expect(
    clippy::float_cmp,
    reason = "every expected size is computed from the same constants by the same operations"
)]

use rc_git_kit::layout_policy::{
    CHANGES_DIFF_FLOOR, CHANGES_FILES_FLOOR, CHANGES_MESSAGE_HEIGHT, CHANGES_STASH_HEIGHT,
    CONTENT_MINIMUM_WIDTH, HISTORY_COMMITS_FLOOR, HISTORY_DETAIL_FLOOR, HISTORY_DETAIL_SHARE,
    HISTORY_DIFF_MIN_WIDTH, HISTORY_DIFF_SHARE, HISTORY_LIST_MIN_WIDTH, Layout,
    SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_MINIMUM_WIDTH, SIDEBAR_WINDOW_WIDTH, SidebarCollapse,
    SidebarFit, Size, WINDOW_MINIMUM_HEIGHT, WINDOW_MINIMUM_WIDTH, history_commits_height,
    history_list_width, is_user_drag, layout, sidebar_width, split_pane,
};

fn size(width: f64, height: f64) -> Size {
    Size { width, height }
}

#[test]
fn wide_window_shows_everything() {
    assert_eq!(
        layout(size(1400.0, 900.0)),
        Layout {
            shows_sidebar: true
        }
    );
}

#[test]
fn narrow_window_hides_sidebar() {
    assert_eq!(
        layout(size(500.0, 600.0)),
        Layout {
            shows_sidebar: false
        }
    );
}

#[test]
fn portrait_window_keeps_the_sidebar_rule_only() {
    assert!(layout(size(900.0, 1400.0)).shows_sidebar);
    assert!(!layout(size(480.0, 1400.0)).shows_sidebar);
}

#[test]
fn sidebar_threshold_is_inclusive() {
    assert!(layout(size(SIDEBAR_WINDOW_WIDTH, 400.0)).shows_sidebar);
    assert!(!layout(size(SIDEBAR_WINDOW_WIDTH - 1.0, 400.0)).shows_sidebar);
}

#[test]
fn sidebar_width_follows_its_content_between_the_bounds() {
    assert_eq!(sidebar_width(300.0), 300.0);
}

#[test]
fn sidebar_width_clamps_to_the_bounds() {
    assert_eq!(sidebar_width(0.0), SIDEBAR_MINIMUM_WIDTH);
    assert_eq!(sidebar_width(4000.0), SIDEBAR_MAXIMUM_WIDTH);
}

#[test]
fn the_sidebar_fits_beside_the_detail_pane_at_the_threshold() {
    const { assert!(SIDEBAR_MAXIMUM_WIDTH < SIDEBAR_WINDOW_WIDTH) };
    const { assert!(SIDEBAR_MINIMUM_WIDTH < SIDEBAR_MAXIMUM_WIDTH) };
}

#[test]
fn the_sidebar_opens_only_where_both_pane_floors_and_the_divider_fit() {
    const { assert!(SIDEBAR_WINDOW_WIDTH >= SIDEBAR_MINIMUM_WIDTH + CONTENT_MINIMUM_WIDTH + 1.0) };
}

#[test]
fn the_smallest_window_holds_the_detail_pane_and_can_collapse_the_sidebar() {
    const { assert!(WINDOW_MINIMUM_WIDTH >= CONTENT_MINIMUM_WIDTH) };
    const { assert!(WINDOW_MINIMUM_WIDTH < SIDEBAR_WINDOW_WIDTH) };
    assert!(!layout(size(WINDOW_MINIMUM_WIDTH, WINDOW_MINIMUM_HEIGHT)).shows_sidebar);
}

#[test]
fn the_changes_floors_fit_the_smallest_window() {
    const { assert!(CHANGES_DIFF_FLOOR > 0.0) };
    const { assert!(CHANGES_DIFF_FLOOR <= WINDOW_MINIMUM_HEIGHT) };
    let commit_column = CHANGES_FILES_FLOOR + CHANGES_STASH_HEIGHT + CHANGES_MESSAGE_HEIGHT;
    const { assert!(CHANGES_FILES_FLOOR > 0.0) };
    assert!(commit_column <= WINDOW_MINIMUM_HEIGHT);
}

#[test]
fn split_pane_keeps_a_divider_both_floors_allow() {
    assert_eq!(split_pane(800.0, 300.0, 100.0, 150.0), 300.0);
}

#[test]
fn split_pane_lifts_a_divider_off_the_first_floor() {
    assert_eq!(split_pane(800.0, 10.0, 100.0, 150.0), 100.0);
}

#[test]
fn split_pane_keeps_the_second_pane_off_zero() {
    // The poisoned autosave: the divider parked at the bottom, the diff pane at zero.
    assert_eq!(split_pane(763.0, 763.0, 100.0, 150.0), 613.0);
}

#[test]
fn split_pane_shares_space_too_small_for_both_floors() {
    let first = split_pane(50.0, 50.0, 100.0, 150.0);
    assert_eq!(first, 20.0);
    assert_eq!(50.0 - first, 30.0);
}

#[test]
fn split_pane_has_no_negative_or_zero_space_case() {
    assert_eq!(split_pane(0.0, 400.0, 100.0, 150.0), 0.0);
    assert_eq!(split_pane(-10.0, 400.0, 100.0, 150.0), 0.0);
}

#[test]
fn split_pane_fits_exactly_both_floors() {
    assert_eq!(split_pane(250.0, 10.0, 100.0, 150.0), 100.0);
}

#[test]
fn the_history_column_floors_leave_both_panes_usable() {
    let available = HISTORY_COMMITS_FLOOR + HISTORY_DETAIL_FLOOR;
    let commits = history_commits_height(available, 0.0);
    assert_eq!(commits, HISTORY_COMMITS_FLOOR);
    assert_eq!(available - commits, HISTORY_DETAIL_FLOOR);
}

#[test]
fn history_keeps_a_detail_height_both_floors_allow() {
    assert_eq!(history_commits_height(800.0, 300.0), 500.0);
    assert_eq!(
        history_commits_height(800.0, HISTORY_DETAIL_FLOOR),
        800.0 - HISTORY_DETAIL_FLOOR
    );
}

#[test]
fn history_opens_an_unsized_detail_at_its_share() {
    // Hidden until a commit opened: the detail arrives at zero, or just under its floor.
    let expected = 1000.0 - 1000.0 * HISTORY_DETAIL_SHARE;
    assert_eq!(history_commits_height(1000.0, 0.0), expected);
    assert_eq!(
        history_commits_height(1000.0, HISTORY_DETAIL_FLOOR - 1.0),
        expected
    );
}

#[test]
fn history_keeps_the_commit_list_off_its_floor() {
    // A detail dragged or restored taller than the column leaves the list its floor.
    assert_eq!(history_commits_height(600.0, 900.0), HISTORY_COMMITS_FLOOR);
}

#[test]
fn history_keeps_a_diff_width_both_floors_allow() {
    assert_eq!(history_list_width(1000.0, 400.0), 600.0);
    assert_eq!(
        history_list_width(1000.0, HISTORY_DIFF_MIN_WIDTH),
        1000.0 - HISTORY_DIFF_MIN_WIDTH
    );
}

#[test]
fn history_opens_an_unsized_diff_at_its_share() {
    // Hidden until a commit opened: the diff arrives at zero, or just under its floor.
    let expected = 1000.0 - 1000.0 * HISTORY_DIFF_SHARE;
    assert_eq!(history_list_width(1000.0, 0.0), expected);
    assert_eq!(
        history_list_width(1000.0, HISTORY_DIFF_MIN_WIDTH - 1.0),
        expected
    );
}

#[test]
fn history_keeps_the_list_off_its_width_floor() {
    // A diff dragged or restored wider than the split leaves the list its floor.
    assert_eq!(history_list_width(900.0, 900.0), HISTORY_LIST_MIN_WIDTH);
}

#[test]
fn history_keeps_both_panes_off_zero_in_a_narrow_content_pane() {
    // The narrowest content pane beside the sidebar is far below both width floors.
    let available = CONTENT_MINIMUM_WIDTH - 1.0;
    let list = history_list_width(available, 500.0);
    assert!(list > 0.0);
    assert!(available - list > 0.0);
    let share = HISTORY_LIST_MIN_WIDTH / (HISTORY_LIST_MIN_WIDTH + HISTORY_DIFF_MIN_WIDTH);
    assert!((list - available * share).abs() < 0.001);
}

const WIDE: Layout = Layout {
    shows_sidebar: true,
};
const NARROW: Layout = Layout {
    shows_sidebar: false,
};

#[test]
fn a_policy_collapse_reopens_when_the_window_widens() {
    let mut collapse = SidebarCollapse::new();
    assert_eq!(collapse.apply(NARROW, Some(WIDE), false), Some(true));
    assert!(collapse.collapsed_by_policy());
    assert_eq!(collapse.apply(WIDE, Some(NARROW), true), Some(false));
    assert!(!collapse.collapsed_by_policy());
}

#[test]
fn a_sidebar_the_user_closed_after_a_policy_collapse_stays_closed() {
    let mut collapse = SidebarCollapse::new();
    assert_eq!(collapse.apply(NARROW, Some(WIDE), false), Some(true));
    collapse.user_toggled(); // opened by hand while narrow
    collapse.user_toggled(); // closed by hand again
    assert_eq!(collapse.apply(WIDE, Some(NARROW), true), None);
}

#[test]
fn a_manual_collapse_while_wide_never_reopens() {
    let mut collapse = SidebarCollapse::new();
    assert_eq!(collapse.apply(WIDE, None, false), None);
    collapse.user_toggled();
    assert_eq!(collapse.apply(NARROW, Some(WIDE), true), None);
    assert_eq!(collapse.apply(WIDE, Some(NARROW), true), None);
}

#[test]
fn a_policy_collapse_stays_while_the_window_stays_wide() {
    let mut collapse = SidebarCollapse::new();
    assert_eq!(collapse.apply(NARROW, Some(WIDE), false), Some(true));
    assert_eq!(
        collapse.apply(WIDE, Some(WIDE), true),
        None,
        "no widening happened"
    );
    assert_eq!(collapse.apply(WIDE, None, true), None, "no previous layout");
}

#[test]
fn only_a_divider_drag_pins_the_sidebar_width() {
    assert!(is_user_drag(Some(1)), "a drag");
    assert!(!is_user_drag(Some(0)), "a programmatic divider move");
    assert!(!is_user_drag(None), "a window resize, or no user info");
}

#[test]
fn fit_applies_only_to_an_open_unpinned_sidebar_showing_files() {
    assert!(SidebarFit::applies(false, true, false));
    assert!(!SidebarFit::applies(true, true, false));
    assert!(!SidebarFit::applies(false, false, false));
    assert!(!SidebarFit::applies(false, true, true));
}

/// Feeds a fit one state per step, as (`shows_files`, `is_collapsed`), and returns whether
/// each step asked the tree to measure again.
fn measures(steps: &[(bool, bool)], is_pinned: bool) -> Vec<bool> {
    let mut fit = SidebarFit::new();
    steps
        .iter()
        .map(|&(shows_files, is_collapsed)| fit.update(is_pinned, shows_files, is_collapsed))
        .collect()
}

#[test]
fn returning_to_files_measures_again_once() {
    let steps = [(true, false), (false, false), (true, false), (true, false)];
    assert_eq!(measures(&steps, false), [true, false, true, false]);
}

#[test]
fn reopening_the_sidebar_measures_again() {
    let steps = [(true, false), (true, true), (true, false)];
    assert_eq!(measures(&steps, false), [true, false, true]);
}

#[test]
fn reopening_the_sidebar_outside_files_waits_for_files() {
    let steps = [(false, true), (false, false), (true, false)];
    assert_eq!(measures(&steps, false), [false, false, true]);
}

#[test]
fn a_pinned_sidebar_never_measures_again() {
    let steps = [
        (true, false),
        (false, false),
        (true, false),
        (true, true),
        (true, false),
    ];
    assert_eq!(measures(&steps, true), [false, false, false, false, false]);
}

#[test]
fn the_fit_remembers_whether_it_applied() {
    let mut fit = SidebarFit::new();
    assert!(!fit.is_applied());
    fit.update(false, true, false);
    assert!(fit.is_applied());
    fit.update(false, false, false);
    assert!(!fit.is_applied());
}
