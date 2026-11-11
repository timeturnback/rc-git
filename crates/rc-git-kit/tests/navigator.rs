//! Port of `NavigatorTests.swift`: what the app-wide back/forward list records, across tabs
//! and restores. A tab is named here by a small number, as a window id would name it.

use rc_git_kit::back_forward::{
    BrowsedTree, BrowserLocation, ContentMode, Direction, NavigationStop,
};
use rc_git_kit::navigator::Navigator;

const ONE: u32 = 1;
const TWO: u32 = 2;

fn stop(repository: &str, file: &str) -> NavigationStop {
    NavigationStop {
        repository_path: repository.to_owned(),
        mode: ContentMode::Files,
        browser: BrowserLocation {
            tree: BrowsedTree::WorkingTree,
            file_path: Some(file.to_owned()),
            ..BrowserLocation::default()
        },
    }
}

fn one(file: &str) -> NavigationStop {
    stop("/one", file)
}

fn two(file: &str) -> NavigationStop {
    stop("/two", file)
}

#[test]
fn a_background_tabs_report_is_ignored() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    let recorded = navigator.report(two("b"), &TWO);
    assert!(!recorded);
    assert_eq!(navigator.list().stops(), [one("a")]);
}

#[test]
fn a_tab_with_no_stop_yet_records_once_in_front() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, None);
    assert!(navigator.report(one("a"), &ONE));
    assert_eq!(navigator.list().stops(), [one("a")]);
}

#[test]
fn a_report_during_a_restore_is_ignored_and_forward_survives() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    navigator.report(one("b"), &ONE);
    let back = navigator.go(Direction::Back).unwrap();
    navigator.begin_restore(&ONE);
    let recorded = navigator.report(one("halfway"), &ONE);
    navigator.report(back.clone(), &ONE);
    navigator.end_restore();

    assert!(!recorded);
    assert_eq!(back, one("a"));
    assert_eq!(navigator.list().stops(), [one("a"), one("b")]);
    assert!(navigator.can_go(Direction::Forward));
    assert_eq!(navigator.go(Direction::Forward), Some(one("b")));
}

#[test]
fn a_report_after_the_restore_ends_is_recorded() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    navigator.report(one("b"), &ONE);
    assert!(navigator.go(Direction::Back).is_some());
    navigator.begin_restore(&ONE);
    navigator.end_restore();
    assert!(navigator.report(one("c"), &ONE));
    assert_eq!(navigator.list().stops(), [one("a"), one("c")]);
}

#[test]
fn a_restore_brings_its_tab_to_the_front() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    navigator.tab_came_to_front(&TWO, Some(two("b")));
    assert!(navigator.go(Direction::Back).is_some());
    navigator.begin_restore(&ONE);
    navigator.end_restore();
    assert!(navigator.report(one("c"), &ONE));
    assert!(!navigator.report(two("d"), &TWO));
}

#[test]
fn switching_tabs_records_the_new_tabs_stop() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    assert!(navigator.tab_came_to_front(&TWO, Some(two("b"))));
    assert_eq!(navigator.list().stops(), [one("a"), two("b")]);
    assert!(navigator.report(two("c"), &TWO));
    assert!(!navigator.report(one("d"), &ONE));
}

#[test]
fn the_same_tab_becoming_main_again_records_nothing() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    navigator.report(one("b"), &ONE);
    assert!(navigator.go(Direction::Back).is_some());
    assert!(!navigator.tab_came_to_front(&ONE, Some(one("b"))));
    assert_eq!(navigator.list().current(), Some(&one("a")));
    assert!(navigator.can_go(Direction::Forward));
}

#[test]
fn closing_a_tab_forgets_its_stops() {
    let mut navigator = Navigator::new();
    navigator.tab_came_to_front(&ONE, Some(one("a")));
    navigator.tab_came_to_front(&TWO, Some(two("b")));
    navigator.tab_closed(&TWO, Some("/two"));
    assert_eq!(navigator.list().stops(), [one("a")]);
    assert!(
        !navigator.report(two("c"), &TWO),
        "a closed tab is not in front"
    );
}

#[test]
fn ending_a_restore_that_never_began_leaves_reports_recorded() {
    let mut navigator = Navigator::new();
    navigator.end_restore();
    navigator.tab_came_to_front(&ONE, None);
    assert!(navigator.report(one("a"), &ONE));
}
