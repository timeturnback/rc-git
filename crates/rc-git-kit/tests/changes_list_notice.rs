//! Ported from `ChangesListNoticeTests.swift`: what the change list says in place of rows,
//! and whether its rows are current.

use rc_git_core::bstr::BString;
use rc_git_core::{EntryKind, StatusEntry, StatusReport, WorktreeStatus};
use rc_git_kit::changes::{CLEAN_PLACEHOLDER, ChangesState, SCANNING_PLACEHOLDER};

fn entry() -> StatusEntry {
    StatusEntry {
        path: BString::from("a.txt"),
        kind: EntryKind::Blob,
        status: WorktreeStatus::Modified,
        oid: None,
        size: 1,
    }
}

fn loaded(entries: Vec<StatusEntry>) -> ChangesState {
    ChangesState::Loaded(StatusReport {
        scanned: entries.len(),
        entries,
        ..StatusReport::default()
    })
}

#[test]
fn first_scan_says_scanning_not_clean() {
    let notice = ChangesState::Loading.notice(false);
    assert_eq!(notice.placeholder.as_deref(), Some(SCANNING_PLACEHOLDER));
    assert!(!notice.is_current);
}

#[test]
fn before_any_scan_says_scanning() {
    let notice = ChangesState::Idle.notice(false);
    assert_eq!(notice.placeholder.as_deref(), Some(SCANNING_PLACEHOLDER));
    assert!(!notice.is_current);
}

#[test]
fn leftover_rows_stay_but_are_not_current() {
    for state in [ChangesState::Idle, ChangesState::Loading] {
        let notice = state.notice(true);
        assert_eq!(notice.placeholder, None, "{state:?}");
        assert!(!notice.is_current, "{state:?}");
    }
}

#[test]
fn only_an_empty_loaded_report_says_clean() {
    let notice = loaded(Vec::new()).notice(false);
    assert_eq!(notice.placeholder.as_deref(), Some(CLEAN_PLACEHOLDER));
    assert!(notice.is_current);
}

#[test]
fn loaded_report_with_entries_has_no_placeholder() {
    let notice = loaded(vec![entry()]).notice(true);
    assert_eq!(notice.placeholder, None);
    assert!(notice.is_current);
}

#[test]
fn failure_shows_its_message() {
    let notice = ChangesState::Failed("status failed".to_owned()).notice(false);
    assert_eq!(notice.placeholder.as_deref(), Some("status failed"));
    assert!(notice.is_current);
}

#[test]
fn only_a_state_with_rows_rebuilds_the_list() {
    assert_eq!(ChangesState::Idle.listed_entries(), None);
    assert_eq!(ChangesState::Loading.listed_entries(), None);
    assert_eq!(loaded(vec![entry()]).listed_entries(), Some(&[entry()][..]));
    assert_eq!(
        ChangesState::Failed("status failed".to_owned()).listed_entries(),
        Some(&[][..])
    );
}
