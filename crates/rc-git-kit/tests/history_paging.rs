//! `LogPager` and `HistoryPager`: a pager that still stands where the cached pages end resumes
//! its walk, and any other opens a new one. One page at a time is the budget, so the fake's
//! page counters carry the assertions.

use rc_git_core::{CommitFilter, Oid};
use rc_git_kit::fake::{FakeRepository, commit_summary, oid};
use rc_git_kit::history_paging::{HistoryPager, LogPager, PageRequest};

const TIP: u8 = 0x10;

fn history(count: u8) -> Vec<rc_git_core::CommitSummary> {
    (0..count)
        .map(|index| commit_summary(oid(0x80 + index), &[], &format!("c{index}")))
        .collect()
}

fn titles(commits: &[rc_git_core::CommitSummary]) -> Vec<String> {
    commits.iter().map(|commit| commit.title.clone()).collect()
}

fn path_filter() -> CommitFilter {
    CommitFilter {
        path: Some("src/lib.rs".to_owned()),
        ..CommitFilter::default()
    }
}

fn fake_with_history() -> FakeRepository {
    let repo = FakeRepository::new();
    let tip: Oid = oid(TIP);
    repo.state().commits.insert(tip, history(5));
    repo.state()
        .path_histories
        .insert((tip, "src/lib.rs".to_owned()), history(5));
    repo
}

#[test]
fn a_log_pager_standing_at_the_cache_end_resumes_its_cursor() {
    let repo = fake_with_history();
    let (pager, first) = LogPager::page(None, &repo, oid(TIP), 0, 2).unwrap();
    assert_eq!(titles(&first), ["c0", "c1"]);
    let (pager, second) = LogPager::page(Some(pager), &repo, oid(TIP), 2, 2).unwrap();
    assert_eq!(titles(&second), ["c2", "c3"]);
    assert_eq!(
        repo.state().log_calls,
        2,
        "one page per call, never a re-walk"
    );

    // A page the model dropped left the cursor past the cache: the next page reopens and
    // passes over the cached commits once.
    let (_, again) = LogPager::page(Some(pager), &repo, oid(TIP), 2, 2).unwrap();
    assert_eq!(titles(&again), ["c2", "c3"]);
    assert_eq!(repo.state().log_calls, 4);
}

#[test]
fn a_log_that_ends_before_the_start_has_nothing_after_it() {
    let repo = fake_with_history();
    let (_, page) = LogPager::page(None, &repo, oid(TIP), 9, 2).unwrap();
    assert!(page.is_empty());
}

#[test]
fn a_missing_tip_fails_the_page() {
    let repo = FakeRepository::new();
    let error = LogPager::page(None, &repo, oid(1), 0, 2).unwrap_err();
    assert!(error.message().contains("missing"), "{error}");
}

#[test]
fn a_history_pager_resumes_and_reopens_by_examined_commits() {
    let repo = fake_with_history();
    let request = |start| PageRequest {
        start,
        limit: 10,
        max_scan: 2,
    };
    let (pager, first) =
        HistoryPager::page(None, &repo, oid(TIP), path_filter(), request(0)).unwrap();
    assert_eq!(first.scanned, 2);
    assert!(!first.complete);
    let (pager, second) =
        HistoryPager::page(Some(pager), &repo, oid(TIP), path_filter(), request(2)).unwrap();
    assert_eq!(titles(&second.commits), ["c2", "c3"]);
    assert_eq!(repo.state().path_history_calls, 2);

    let (_, reopened) =
        HistoryPager::page(Some(pager), &repo, oid(TIP), path_filter(), request(2)).unwrap();
    assert_eq!(titles(&reopened.commits), ["c2", "c3"]);
    assert_eq!(
        repo.state().path_history_calls,
        4,
        "a mismatch opens a new walk and examines the first commits again in one page"
    );
}

#[test]
fn a_failed_page_loses_the_place_so_the_next_reopens() {
    let repo = fake_with_history();
    let request = |start| PageRequest {
        start,
        limit: 10,
        max_scan: 2,
    };
    let (pager, _) = HistoryPager::page(None, &repo, oid(TIP), path_filter(), request(0)).unwrap();
    repo.state().path_history_fail_from = Some(2);
    let failed = HistoryPager::page(
        Some(pager.clone()),
        &repo,
        oid(TIP),
        path_filter(),
        request(2),
    );
    assert!(failed.is_err());
    repo.state().path_history_fail_from = None;
    let calls = repo.state().path_history_calls;
    let (_, page) =
        HistoryPager::page(Some(pager), &repo, oid(TIP), path_filter(), request(2)).unwrap();
    assert_eq!(titles(&page.commits), ["c2", "c3"]);
    assert_eq!(
        repo.state().path_history_calls,
        calls + 2,
        "the lost walk was replaced"
    );
}

#[test]
fn a_filtered_walk_that_ends_before_the_start_is_complete_and_empty() {
    let repo = fake_with_history();
    let filter = CommitFilter {
        message: Some("c".to_owned()),
        ..CommitFilter::default()
    };
    let request = PageRequest {
        start: 9,
        limit: 10,
        max_scan: 5,
    };
    let (_, page) = HistoryPager::page(None, &repo, oid(TIP), filter, request).unwrap();
    assert!(page.commits.is_empty());
    assert!(page.complete);
    assert_eq!(
        repo.state().search_max_scans,
        [9],
        "the walk ran out while passing over the first nine, so no page was asked for"
    );
}
