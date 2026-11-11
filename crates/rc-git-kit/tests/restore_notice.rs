//! Ported from `RestoreNoticeTests.swift`: the banner shown when saved tabs did not reopen.

use rc_git_kit::restore_notice::RestoreNotice;

fn paths(list: &[&str]) -> Vec<String> {
    list.iter().map(|path| (*path).to_owned()).collect()
}

#[test]
fn every_tab_opened_shows_no_notice() {
    assert_eq!(RestoreNotice::make(&[], &paths(&["/r/a"])), None);
}

#[test]
fn one_dropped_tab_is_named_as_removed() {
    let notice = RestoreNotice::make(&paths(&["/gone/repo"]), &paths(&["/r/a"])).unwrap();
    assert_eq!(
        notice.summary,
        "Could not reopen 1 saved tab: repo (removed from saved tabs)"
    );
    assert_eq!(notice.detail, "/gone/repo: removed from saved tabs");
}

#[test]
fn a_kept_tab_says_it_is_retried_and_each_failure_is_listed() {
    let notice = RestoreNotice::make(
        &paths(&["/Volumes/Ext/x", "/gone/y"]),
        &paths(&["/r/a", "/Volumes/Ext/x"]),
    )
    .unwrap();
    assert_eq!(
        notice.summary,
        "Could not reopen 2 saved tabs: x (not reachable, will retry next launch), \
         y (removed from saved tabs)"
    );
    assert_eq!(
        notice.detail,
        "/Volumes/Ext/x: not reachable, will retry next launch\n/gone/y: removed from saved tabs"
    );
}

#[test]
fn a_root_path_is_named_by_the_whole_path() {
    let notice = RestoreNotice::make(&paths(&["/"]), &[]).unwrap();
    assert_eq!(
        notice.summary,
        "Could not reopen 1 saved tab: / (removed from saved tabs)"
    );
}

#[test]
fn a_trailing_slash_still_names_the_folder() {
    let notice = RestoreNotice::make(&paths(&["/gone/repo/"]), &[]).unwrap();
    assert!(notice.summary.ends_with(": repo (removed from saved tabs)"));
}
