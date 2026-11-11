//! The app-wide back/forward list: visits, steps, the stop limit, closing a tab, and the
//! commit detail's back arrow. Port of `BackForwardListTests.swift`.
#![expect(
    clippy::unwrap_used,
    reason = "the oid helper builds ids from fixed 20-byte seeds"
)]

use rc_git_core::Oid;
use rc_git_kit::back_forward::{
    BackForwardList, BrowsedTree, BrowserLocation, ContentMode, Direction, LIMIT, NavigationStop,
};

fn oid(seed: u8) -> Oid {
    Oid::from_bytes(&[seed; 20]).unwrap()
}

fn stop_in(
    repository: &str,
    mode: ContentMode,
    file: Option<&str>,
    commit: Option<Oid>,
) -> NavigationStop {
    NavigationStop {
        repository_path: repository.to_owned(),
        mode,
        browser: BrowserLocation {
            tree: BrowsedTree::WorkingTree,
            file_path: file.map(str::to_owned),
            detail_commit: commit,
            ..BrowserLocation::default()
        },
    }
}

fn file(name: &str) -> NavigationStop {
    stop_in("/one", ContentMode::Files, Some(name), None)
}

fn file_in(repository: &str, name: &str) -> NavigationStop {
    stop_in(repository, ContentMode::Files, Some(name), None)
}

fn history(repository: &str, commit: Option<Oid>) -> NavigationStop {
    stop_in(repository, ContentMode::History, None, commit)
}

fn list(stops: &[NavigationStop]) -> BackForwardList {
    let mut list = BackForwardList::new();
    for stop in stops {
        list.visit(stop.clone());
    }
    list
}

#[test]
fn an_empty_list_goes_nowhere() {
    let mut list = BackForwardList::new();
    assert_eq!(list.current(), None);
    assert!(!list.can_go(Direction::Back));
    assert!(!list.can_go(Direction::Forward));
    assert_eq!(list.go(Direction::Back), None);
    assert_eq!(list.go(Direction::Forward), None);
}

#[test]
fn a_visit_becomes_the_current_stop() {
    let list = list(&[file("a"), file("b")]);
    assert_eq!(list.stops().len(), 2);
    assert_eq!(list.current(), Some(&file("b")));
    assert!(list.can_go(Direction::Back));
    assert!(!list.can_go(Direction::Forward));
}

#[test]
fn visiting_the_current_stop_records_nothing() {
    let mut list = list(&[file("a"), file("b")]);
    list.visit(file("b"));
    assert_eq!(list.stops().len(), 2);
    list.go(Direction::Back);
    list.visit(file("a"));
    assert_eq!(list.stops().len(), 2, "a no-op visit keeps the stops ahead");
    assert!(list.can_go(Direction::Forward));
}

#[test]
fn stepping_moves_the_mark_and_stops_at_either_end() {
    let mut list = list(&[file("a"), file("b"), file("c")]);
    assert_eq!(list.go(Direction::Back), Some(&file("b")));
    assert_eq!(list.go(Direction::Back), Some(&file("a")));
    assert_eq!(list.go(Direction::Back), None);
    assert_eq!(list.current(), Some(&file("a")));
    assert_eq!(list.go(Direction::Forward), Some(&file("b")));
    assert_eq!(list.go(Direction::Forward), Some(&file("c")));
    assert_eq!(list.go(Direction::Forward), None);
    assert_eq!(list.stops().len(), 3, "stepping never drops a stop");
}

#[test]
fn a_visit_after_stepping_back_drops_the_stops_ahead() {
    let mut list = list(&[file("a"), file("b"), file("c")]);
    list.go(Direction::Back);
    list.go(Direction::Back);
    list.visit(file("d"));
    assert_eq!(list.stops(), [file("a"), file("d")]);
    assert_eq!(list.current(), Some(&file("d")));
    assert!(!list.can_go(Direction::Forward));
}

#[test]
fn the_oldest_stops_fall_off_past_the_limit() {
    let mut list = BackForwardList::new();
    for index in 0..LIMIT + 5 {
        list.visit(file(&index.to_string()));
    }
    assert_eq!(list.stops().len(), LIMIT);
    assert_eq!(list.stops().first(), Some(&file("5")));
    assert_eq!(list.current(), Some(&file(&(LIMIT + 4).to_string())));
}

#[test]
fn closing_a_tab_drops_its_stops_and_keeps_the_mark() {
    let mut list = list(&[
        file_in("/one", "a"),
        file_in("/two", "b"),
        file_in("/one", "c"),
        file_in("/two", "d"),
    ]);
    list.go(Direction::Back);
    list.remove_stops("/two");
    assert_eq!(list.stops(), [file_in("/one", "a"), file_in("/one", "c")]);
    assert_eq!(list.current(), Some(&file_in("/one", "c")));
}

#[test]
fn dropping_the_current_stop_falls_back_to_the_one_before() {
    let mut list = list(&[
        file_in("/one", "a"),
        file_in("/two", "b"),
        file_in("/one", "c"),
    ]);
    list.go(Direction::Back);
    list.remove_stops("/two");
    assert_eq!(list.current(), Some(&file_in("/one", "a")));
    assert!(list.can_go(Direction::Forward));
}

#[test]
fn dropping_the_first_and_current_stop_lands_on_the_first_left() {
    let mut list = list(&[
        file_in("/two", "a"),
        file_in("/one", "b"),
        file_in("/one", "c"),
    ]);
    list.go(Direction::Back);
    list.go(Direction::Back);
    list.remove_stops("/two");
    assert_eq!(list.current(), Some(&file_in("/one", "b")));
    assert!(!list.can_go(Direction::Back));
}

#[test]
fn dropping_every_stop_empties_the_list() {
    let mut list = list(&[file_in("/two", "a"), file_in("/two", "b")]);
    list.remove_stops("/two");
    assert!(list.stops().is_empty());
    assert_eq!(list.current(), None);
    assert!(!list.can_go(Direction::Back));
    list.visit(file_in("/one", "c"));
    assert_eq!(list.current(), Some(&file_in("/one", "c")));
}

#[test]
fn dropping_stops_merges_neighbors_that_became_the_same_place() {
    let mut list = list(&[
        file_in("/one", "a"),
        file_in("/two", "b"),
        file_in("/one", "a"),
    ]);
    list.remove_stops("/two");
    assert_eq!(list.stops(), [file_in("/one", "a")]);
    assert_eq!(list.current(), Some(&file_in("/one", "a")));
    assert!(
        !list.can_go(Direction::Back),
        "a step back must change what is on screen"
    );
}

#[test]
fn the_detail_arrow_needs_another_commit_behind_it_in_the_same_history() {
    let (first, second) = (oid(0x01), oid(0x02));
    let two_commits = list(&[history("/one", Some(first)), history("/one", Some(second))]);
    assert!(two_commits.back_returns_to_commit("/one"));
    assert!(
        !two_commits.back_returns_to_commit("/two"),
        "another tab's stops"
    );

    let from_no_commit = list(&[history("/one", None), history("/one", Some(first))]);
    assert!(!from_no_commit.back_returns_to_commit("/one"));

    let from_files = list(&[
        stop_in("/one", ContentMode::Files, None, Some(first)),
        history("/one", Some(second)),
    ]);
    assert!(!from_files.back_returns_to_commit("/one"));

    let from_another_tab = list(&[history("/two", Some(first)), history("/one", Some(second))]);
    assert!(!from_another_tab.back_returns_to_commit("/one"));

    assert!(!BackForwardList::new().back_returns_to_commit("/one"));
}

#[test]
fn the_detail_arrow_needs_a_different_commit_and_a_history_current_stop() {
    let first = oid(0x01);
    let same_commit = list(&[
        history("/one", Some(first)),
        stop_in("/one", ContentMode::History, Some("x"), Some(first)),
    ]);
    assert!(
        !same_commit.back_returns_to_commit("/one"),
        "the same commit twice"
    );
    let to_files = list(&[
        history("/one", Some(first)),
        stop_in("/one", ContentMode::Files, None, None),
    ]);
    assert!(
        !to_files.back_returns_to_commit("/one"),
        "the current stop is not History"
    );
}

#[test]
fn mouse_buttons_and_swipes_name_a_direction() {
    assert_eq!(Direction::from_mouse_button(3), Some(Direction::Back));
    assert_eq!(Direction::from_mouse_button(4), Some(Direction::Forward));
    assert_eq!(Direction::from_mouse_button(2), None, "the middle button");
    assert_eq!(Direction::from_swipe(1.0), Some(Direction::Back));
    assert_eq!(Direction::from_swipe(-0.4), Some(Direction::Forward));
    assert_eq!(Direction::from_swipe(0.0), None);
}
