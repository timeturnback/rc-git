//! Where a repository opens.

use rc_git_kit::tab_routing::{Route, TabIdentity, by_path, route};

const EMPTY: TabIdentity<'static> = TabIdentity {
    path: None,
    key: None,
};

fn tab(path: &'static str, key: &'static str) -> TabIdentity<'static> {
    TabIdentity {
        path: Some(path),
        key: Some(key),
    }
}

#[test]
fn the_same_path_finds_its_tab_before_any_open() {
    let tabs = [tab("/a", "/a/.git"), tab("/b", "/b/.git")];
    assert_eq!(by_path(&tabs, "/b"), Some(1));
    assert_eq!(by_path(&tabs, "/c"), None);
    assert_eq!(by_path(&[EMPTY], "/a"), None);
}

#[test]
fn another_spelling_of_an_open_repository_finds_its_tab_by_key() {
    let tabs = [EMPTY, tab("/a", "/a/.git")];
    assert_eq!(route(&tabs, "/a/.git"), Route::Existing(1));
}

#[test]
fn a_new_repository_fills_the_first_empty_tab_else_a_new_one() {
    let tabs = [tab("/a", "/a/.git"), EMPTY, EMPTY];
    assert_eq!(route(&tabs, "/b/.git"), Route::Reuse(1));
    assert_eq!(route(&[tab("/a", "/a/.git")], "/b/.git"), Route::New);
    assert_eq!(route(&[], "/b/.git"), Route::New);
}
