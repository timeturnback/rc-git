//! The window tab strip against the fakes: it shows once a tab group holds two windows, on
//! every window of the group, and redraws when the group changes; its tabs share its full width
//! equally whatever their titles; a click selects a tab, its close button closes that window,
//! the plus button asks to open a repository, and a window that takes a repository renames its
//! tab.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Modifiers, Pixels, SystemWindowTab, SystemWindowTabController, TestAppContext,
    VisualTestContext, px,
};

use crate::app::Tab;
use crate::menus::OpenRepository;
use crate::tests::support::{Harness, harness, open_settled, repo};

fn windows(harness: &Harness, cx: &TestAppContext) -> Vec<Tab> {
    harness
        .shell
        .read_with(cx, |shell, _| shell.windows().to_vec())
}

/// Puts `tabs` in one tab group, as `TabHost::join` does natively; the test platform has no
/// native tab groups for the shell to read.
fn group(cx: &mut TestAppContext, tabs: &[Tab]) {
    let members: Vec<SystemWindowTab> = tabs
        .iter()
        .map(|tab| SystemWindowTab::new("rc-git".into(), tab.handle.into()))
        .collect();
    let last = tabs.last().map(Tab::id).expect("a group has a window");
    cx.update(|cx| {
        for tab in tabs {
            SystemWindowTabController::remove_tab(cx, tab.id());
        }
        SystemWindowTabController::add_tab(cx, last, members);
    });
}

/// `tab`'s window, drawn with everything settled, to read bounds from or click in.
fn drawn(tab: &Tab, cx: &mut TestAppContext) -> VisualTestContext {
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window, _| window.refresh());
    visual.run_until_parked();
    visual
}

fn shows_strip(tab: &Tab, cx: &mut TestAppContext) -> bool {
    drawn(tab, cx).debug_bounds("window-tabs").is_some()
}

fn titles(tab: &Tab, cx: &TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        cx.global::<SystemWindowTabController>()
            .tabs(tab.id())
            .map(|tabs| tabs.iter().map(|tab| tab.title.to_string()).collect())
            .unwrap_or_default()
    })
}

#[gpui::test]
fn a_lone_window_draws_no_tab_strip(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    open_settled(&harness, cx, "/repo/a");
    let tabs = windows(&harness, cx);
    group(cx, &tabs);
    assert!(!shows_strip(&tabs[0], cx));
}

#[gpui::test]
fn every_window_of_a_two_tab_group_draws_the_strip(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo()), ("/repo/b", repo())]);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let tabs = windows(&harness, cx);
    assert_eq!(tabs.len(), 2);
    assert!(
        !shows_strip(&tabs[0], cx),
        "ungrouped windows draw no strip"
    );
    group(cx, &tabs);
    assert!(shows_strip(&tabs[0], cx));
    assert!(shows_strip(&tabs[1], cx));
}

#[gpui::test]
fn a_window_that_takes_a_repository_names_its_tab(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo()), ("/repo/b", repo())]);
    open_settled(&harness, cx, "/repo/a");
    harness.shell.update(cx, crate::app::Shell::new_window);
    let tabs = windows(&harness, cx);
    group(cx, &tabs);
    assert_eq!(titles(&tabs[0], cx), ["rc-git", "rc-git"]);
    open_settled(&harness, cx, "/repo/b");
    assert_eq!(titles(&tabs[0], cx), ["rc-git", "b"]);
}

/// The bounds of the elements named by `selectors` in `visual`'s drawn window.
fn bounds_of<const N: usize>(
    visual: &mut VisualTestContext,
    selectors: [&'static str; N],
) -> [Bounds<Pixels>; N] {
    selectors.map(|selector| {
        visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
    })
}

#[gpui::test]
fn the_tabs_share_the_full_width_equally(cx: &mut TestAppContext) {
    let long = "/repo/a-repository-whose-name-is-much-longer";
    let harness = harness(cx, &[("/repo/a", repo()), (long, repo())]);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, long);
    let tabs = windows(&harness, cx);
    group(cx, &tabs);
    cx.update(|cx| {
        SystemWindowTabController::update_tab_title(cx, tabs[0].id(), "a".into());
        SystemWindowTabController::update_tab_title(cx, tabs[1].id(), long.into());
    });
    let [strip, first, second, plus] = bounds_of(
        &mut drawn(&tabs[0], cx),
        [
            "window-tabs",
            "window-tab-0",
            "window-tab-1",
            "open-repository-tab",
        ],
    );
    assert!(
        (first.size.width - second.size.width).abs() < px(1.),
        "tabs {first:?} and {second:?} differ in width"
    );
    assert!(
        plus.left() >= second.right(),
        "the plus follows the last tab"
    );
    assert!(
        plus.left() - second.right() < px(16.),
        "the tabs reach the plus: last tab {second:?}, plus {plus:?}"
    );
    assert!(
        strip.right() - plus.right() < px(16.),
        "the tabs and the plus fill the strip: {strip:?}, plus {plus:?}"
    );
    assert!(
        first.left() - strip.left() < px(16.),
        "the tabs start at the strip's edge"
    );
}

#[gpui::test]
fn the_plus_button_asks_to_open_a_repository(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo()), ("/repo/b", repo())]);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let tabs = windows(&harness, cx);
    group(cx, &tabs);
    let asked = Rc::new(Cell::new(0));
    let seen = Rc::clone(&asked);
    cx.update(|cx| {
        cx.on_action(move |_: &OpenRepository, _| seen.set(seen.get() + 1));
    });
    let mut visual = drawn(&tabs[0], cx);
    let [plus] = bounds_of(&mut visual, ["open-repository-tab"]);
    visual.simulate_click(plus.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(asked.get(), 1);
}

#[gpui::test]
fn a_window_redraws_its_strip_when_its_group_changes(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo()), ("/repo/b", repo())]);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let tabs = windows(&harness, cx);
    let mut visual = drawn(&tabs[0], cx);
    assert!(visual.debug_bounds("window-tabs").is_none());
    group(&mut visual, &tabs);
    visual.run_until_parked();
    assert!(visual.debug_bounds("window-tabs").is_some());
}

#[gpui::test]
fn a_tab_close_button_closes_that_tab_s_window(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo()), ("/repo/b", repo())]);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let tabs = windows(&harness, cx);
    group(cx, &tabs);
    let mut visual = drawn(&tabs[0], cx);
    let [close] = bounds_of(&mut visual, ["close-window-tab-1"]);
    visual.simulate_click(close.center(), Modifiers::none());
    visual.run_until_parked();
    let left: Vec<_> = cx.update(|cx| {
        cx.windows()
            .iter()
            .map(gpui::AnyWindowHandle::window_id)
            .collect()
    });
    assert_eq!(left, [tabs[0].id()]);
}

#[gpui::test]
fn clicking_a_tab_brings_its_window_forward(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo()), ("/repo/b", repo())]);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let tabs = windows(&harness, cx);
    group(cx, &tabs);
    let mut visual = drawn(&tabs[0], cx);
    visual.update(|window, _| window.activate_window());
    visual.run_until_parked();
    let [second] = bounds_of(&mut visual, ["window-tab-1"]);
    visual.simulate_click(second.center(), Modifiers::none());
    visual.run_until_parked();
    let active = cx.update(|cx| cx.active_window().map(|window| window.window_id()));
    assert_eq!(active, Some(tabs[1].id()));
}
