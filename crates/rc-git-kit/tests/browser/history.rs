//! Port of `BrowserHistoryTests.swift`, `BrowserHistoryTests`: history paging, path scope,
//! commit search, commit browsing, and the refs refresh the watcher drives.

use std::sync::Arc;
use std::time::Duration;

use gpui::{Entity, TestAppContext};
use rc_git_core::{CommitFilter, CommitSummary, Head, Oid, RefKind, RefListing};
use rc_git_kit::back_forward::HistoryScope;
use rc_git_kit::browser::{
    BrowserEvent, BrowserModel, HISTORY_CACHE_LIMIT, HISTORY_PAGE_SIZE, SEARCH_FIRST_SCAN_CHUNK,
};
use rc_git_kit::fake::{FakeRepository, branch, commit_summary, oid};

use crate::support::{arrived, attach, count, errors, events, gate, model, record, until};

const MAIN_COMMIT: u8 = 0x11;
const MAIN_TREE: u8 = 0x12;
const FEATURE_COMMIT: u8 = 0x21;
const FEATURE_TREE: u8 = 0x22;

fn repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![
                branch("feature", oid(FEATURE_COMMIT), RefKind::LocalBranch),
                branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch),
            ],
            broken: Vec::new(),
        };
        state.head = head("refs/heads/main", Some(oid(MAIN_COMMIT)));
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.tree_of.insert(oid(FEATURE_COMMIT), oid(FEATURE_TREE));
        state.trees.insert(oid(MAIN_TREE), Vec::new());
        state.trees.insert(oid(FEATURE_TREE), Vec::new());
        let main = (0..150u8)
            .map(|index| commit_summary(oid(100 + index % 100), &[], &format!("main {index}")))
            .collect();
        state.commits.insert(oid(MAIN_COMMIT), main);
        state.commits.insert(
            oid(FEATURE_COMMIT),
            vec![commit_summary(oid(FEATURE_COMMIT), &[], "feature 0")],
        );
    }
    repo
}

fn head(name: &str, target: Option<Oid>) -> Head {
    Head {
        ref_name: Some(name.to_owned()),
        target,
        exact_ref_name: true,
    }
}

fn touching(count: usize, prefix: &str) -> Vec<CommitSummary> {
    (0..count)
        .map(|index| {
            let seed = u8::try_from(index % 250).unwrap_or(0);
            commit_summary(oid(seed), &[], &format!("{prefix} {index}"))
        })
        .collect()
}

fn scope(repo: &FakeRepository, path: &str, commits: Vec<CommitSummary>) {
    repo.state()
        .path_histories
        .insert((oid(MAIN_COMMIT), path.to_owned()), commits);
}

fn message(text: &str) -> CommitFilter {
    CommitFilter {
        message: Some(text.to_owned()),
        ..CommitFilter::default()
    }
}

async fn more(cx: &mut TestAppContext, model: &Entity<BrowserModel>) {
    model
        .update(cx, rc_git_kit::browser::BrowserModel::load_more_history)
        .await;
}

async fn show_path(cx: &mut TestAppContext, model: &Entity<BrowserModel>, path: &str) {
    model
        .update(cx, |m, cx| m.show_path_history(path, true, cx))
        .await;
}

async fn search(cx: &mut TestAppContext, model: &Entity<BrowserModel>, filter: CommitFilter) {
    model.update(cx, |m, cx| m.search(filter, cx)).await;
}

async fn select(cx: &mut TestAppContext, model: &Entity<BrowserModel>, name: &str) {
    let reference = model.read_with(cx, |m, _| {
        m.refs()
            .refs
            .iter()
            .find(|r| r.short_name == name)
            .cloned()
            .unwrap()
    });
    model.update(cx, |m, cx| m.select(reference, cx)).await;
}

async fn refresh(cx: &mut TestAppContext, model: &Entity<BrowserModel>) {
    model
        .update(cx, rc_git_kit::browser::BrowserModel::refresh_refs)
        .await;
}

fn titles(model: &BrowserModel) -> Vec<String> {
    model.history().iter().map(|c| c.title.clone()).collect()
}

fn history_len(cx: &mut TestAppContext, model: &Entity<BrowserModel>) -> usize {
    model.read_with(cx, |m, _| m.history().len())
}

#[gpui::test]
async fn history_loads_in_pages_and_reports_completion(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    assert_eq!(history_len(cx, &model), 0);
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 100);
    assert!(!model.read_with(cx, |m, _| m.history_complete()));
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 150);
    assert!(
        model.read_with(cx, |m, _| m.history_complete()),
        "a short page means the walk is done"
    );
    let calls = repo.state().log_calls;
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 150);
    assert_eq!(
        repo.state().log_calls,
        calls,
        "a complete history never re-fetches"
    );
}

#[gpui::test]
async fn path_scope_filters_and_restores(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    more(cx, &model).await;
    scope(
        &repo,
        "src/a.rs",
        vec![
            commit_summary(oid(0xA1), &[], "touch 1"),
            commit_summary(oid(0xA2), &[], "touch 2"),
        ],
    );
    show_path(cx, &model, "src/a.rs").await;
    model.read_with(cx, |m, _| {
        assert_eq!(
            *m.history_scope(),
            HistoryScope::Path("src/a.rs".to_owned())
        );
        assert_eq!(titles(m), ["touch 1", "touch 2"]);
        assert!(m.history_complete());
    });
    let calls = repo.state().log_calls;
    model.update(cx, rc_git_kit::browser::BrowserModel::clear_history_scope);
    assert_eq!(
        model.read_with(cx, |m, _| m.history_scope().clone()),
        HistoryScope::All
    );
    assert_eq!(
        history_len(cx, &model),
        100,
        "the unfiltered pages come back from the cache"
    );
    assert_eq!(repo.state().log_calls, calls);
}

#[gpui::test]
async fn path_scope_caches_per_tip_and_path(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    more(cx, &model).await;
    scope(&repo, "a", vec![commit_summary(oid(0xA1), &[], "touch a")]);
    scope(&repo, "b", vec![commit_summary(oid(0xB1), &[], "touch b")]);
    show_path(cx, &model, "a").await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["touch a"]);
    show_path(cx, &model, "b").await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["touch b"]);
    let calls = repo.state().path_history_calls;
    show_path(cx, &model, "a").await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["touch a"]);
    assert_eq!(
        repo.state().path_history_calls,
        calls,
        "a complete scope comes back without a walk"
    );
}

#[gpui::test]
async fn history_cache_keeps_only_the_most_recent_tips(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let limit = HISTORY_CACHE_LIMIT.get();
    let tips: Vec<Oid> = (0..=limit)
        .map(|index| oid(0x50 + u8::try_from(index).unwrap()))
        .collect();
    for (index, tip) in tips.iter().enumerate() {
        let mut state = repo.state();
        state.tree_of.insert(*tip, oid(MAIN_TREE));
        state.commits.insert(
            *tip,
            vec![commit_summary(*tip, &[], &format!("tip {index}"))],
        );
    }
    attach(cx, &model, &repo).await;
    for (index, tip) in tips.iter().enumerate() {
        let reference = branch(&format!("tip-{index}"), *tip, RefKind::LocalBranch);
        model.update(cx, |m, cx| m.select(reference, cx)).await;
        more(cx, &model).await;
        assert!(model.read_with(cx, |m, _| m.history_cache_count()) <= limit);
    }
    assert_eq!(model.read_with(cx, |m, _| m.history_cache_count()), limit);

    let calls = repo.state().log_calls;
    let recent = branch("tip-1", tips[1], RefKind::LocalBranch);
    model.update(cx, |m, cx| m.select(recent, cx)).await;
    more(cx, &model).await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["tip 1"]);
    assert_eq!(
        repo.state().log_calls,
        calls,
        "a recent tip comes from the cache"
    );
    let oldest = branch("tip-0", tips[0], RefKind::LocalBranch);
    model.update(cx, |m, cx| m.select(oldest, cx)).await;
    more(cx, &model).await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["tip 0"]);
    assert_eq!(
        repo.state().log_calls,
        calls + 1,
        "the oldest tip was evicted"
    );
    assert_eq!(model.read_with(cx, |m, _| m.history_cache_count()), limit);
}

#[gpui::test]
async fn path_history_cache_keeps_only_the_most_recent_scopes(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let limit = HISTORY_CACHE_LIMIT.get();
    let paths: Vec<String> = (0..=limit).map(|index| format!("file{index}")).collect();
    for path in &paths {
        scope(
            &repo,
            path,
            vec![commit_summary(oid(0xA1), &[], &format!("touch {path}"))],
        );
    }
    attach(cx, &model, &repo).await;
    for path in &paths {
        show_path(cx, &model, path).await;
        assert!(model.read_with(cx, |m, _| m.path_history_cache_count()) <= limit);
    }
    assert_eq!(
        model.read_with(cx, |m, _| m.path_history_cache_count()),
        limit
    );

    let calls = repo.state().path_history_calls;
    show_path(cx, &model, &paths[1]).await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["touch file1"]);
    assert_eq!(
        repo.state().path_history_calls,
        calls,
        "a recent scope comes from the cache"
    );
    show_path(cx, &model, &paths[0]).await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["touch file0"]);
    assert_eq!(
        repo.state().path_history_calls,
        calls + 1,
        "the oldest scope was evicted"
    );
}

#[gpui::test]
async fn path_scope_pages_and_completes(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    scope(&repo, "big", touching(150, "touch"));
    show_path(cx, &model, "big").await;
    assert_eq!(
        history_len(cx, &model),
        100,
        "one page even though more commits match"
    );
    assert!(!model.read_with(cx, |m, _| m.history_complete()));
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 150);
    assert!(model.read_with(cx, |m, _| m.history_complete()));
}

#[gpui::test]
async fn path_scope_walks_in_chunks_until_a_page_is_full(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let commits = touching(150, "touch");
    scope(&repo, "big", commits.clone());
    repo.state().path_matches_per_call = Some(30);
    let calls = repo.state().path_history_calls;
    show_path(cx, &model, "big").await;
    assert_eq!(history_len(cx, &model), 100);
    assert_eq!(
        repo.state().path_history_calls,
        calls + 4,
        "30 + 30 + 30 + 10 matches, each chunk resuming the same walk"
    );
    let expected: Vec<String> = commits[..100].iter().map(|c| c.title.clone()).collect();
    assert_eq!(model.read_with(cx, |m, _| titles(m)), expected);
}

#[gpui::test]
async fn path_scope_change_between_chunks_drops_the_rest_of_the_walk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    scope(&repo, "big", touching(150, "touch"));
    {
        let mut state = repo.state();
        state.path_matches_per_call = Some(30);
        state.path_history_delay = Duration::from_millis(500);
    }
    let slow = model.update(cx, |m, cx| m.show_path_history("big", true, cx));
    let watched = repo.clone();
    until(cx, move || watched.state().path_history_calls >= 1).await;
    model.update(cx, rc_git_kit::browser::BrowserModel::clear_history_scope);
    slow.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(*m.history_scope(), HistoryScope::All);
        assert!(!m.history().iter().any(|c| c.title.starts_with("touch")));
    });
    assert_eq!(
        repo.state().path_history_calls,
        1,
        "the chunk in flight lands nowhere and no later chunk runs"
    );
}

#[gpui::test]
async fn graph_rows_follow_the_paged_history_and_its_cache(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    more(cx, &model).await;
    assert_eq!(model.read_with(cx, |m, _| m.graph_rows().len()), 100);
    more(cx, &model).await;
    assert_eq!(model.read_with(cx, |m, _| m.graph_rows().len()), 150);

    select(cx, &model, "feature").await;
    assert!(
        model.read_with(cx, |m, _| m.graph_rows().len()) <= 1,
        "another tip starts its own graph"
    );
    let calls = repo.state().log_calls;
    select(cx, &model, "main").await;
    assert_eq!(
        model.read_with(cx, |m, _| m.graph_rows().len()),
        150,
        "the cached graph returns with the pages"
    );
    assert_eq!(repo.state().log_calls, calls);

    search(cx, &model, message("main")).await;
    assert!(
        model.read_with(cx, |m, _| m.graph_rows().is_empty()),
        "a filtered list has no topology"
    );
    model.update(cx, rc_git_kit::browser::BrowserModel::clear_search);
    assert_eq!(model.read_with(cx, |m, _| m.graph_rows().len()), 150);
}

#[gpui::test]
async fn search_streams_matches_and_clear_restores_instantly(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    more(cx, &model).await;
    let events = events(cx, &model);
    search(cx, &model, message("main 1")).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.search_results().len(),
            61,
            "main 1, main 10-19, main 100-149"
        );
        assert!(m.search_complete());
        assert!(!m.is_searching());
    });
    assert!(
        count(&events, &BrowserEvent::SearchChanged) >= 2,
        "started, then streamed"
    );
    let calls = repo.state().search_calls;
    model.update(cx, rc_git_kit::browser::BrowserModel::clear_search);
    model.read_with(cx, |m, _| {
        assert!(m.search_filter().is_none());
        assert!(m.search_results().is_empty());
        assert_eq!(m.history().len(), 100, "the paged history is untouched");
    });
    assert_eq!(repo.state().search_calls, calls, "clearing needs no call");
}

#[gpui::test]
async fn search_pages_on_demand(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    search(cx, &model, message("main")).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.search_results().len()),
        100,
        "one page of matches"
    );
    assert!(!model.read_with(cx, |m, _| m.search_complete()));
    model
        .update(
            cx,
            rc_git_kit::browser::BrowserModel::load_more_search_results,
        )
        .await;
    assert_eq!(model.read_with(cx, |m, _| m.search_results().len()), 150);
    assert!(model.read_with(cx, |m, _| m.search_complete()));
}

#[gpui::test]
async fn a_history_page_for_an_earlier_tip_is_dropped(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .delays
        .insert(oid(MAIN_COMMIT), Duration::from_millis(300));
    let slow_page = model.update(cx, rc_git_kit::browser::BrowserModel::load_more_history);
    select(cx, &model, "feature").await;
    slow_page.await;
    cx.run_until_parked();
    repo.state().delays.remove(&oid(MAIN_COMMIT));
    assert_eq!(
        history_len(cx, &model),
        0,
        "main's page never lands on feature"
    );
    more(cx, &model).await;
    assert_eq!(model.read_with(cx, |m, _| titles(m)), ["feature 0"]);
    assert!(model.read_with(cx, |m, _| m.history_complete()));
    select(cx, &model, "main").await;
    assert_eq!(
        history_len(cx, &model),
        0,
        "the dropped page was never cached under main's tip"
    );
}

fn all_contain(seen: &[Vec<String>], needle: &str) -> bool {
    seen.iter()
        .all(|titles| titles.iter().all(|t| t.contains(needle)))
}

#[gpui::test]
async fn a_newer_search_drops_the_older_walk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .delays
        .insert(oid(MAIN_COMMIT), Duration::from_millis(300));
    let seen = record(cx, &model, BrowserEvent::SearchChanged, |m| {
        m.search_results()
            .iter()
            .map(|c| c.title.clone())
            .collect::<Vec<_>>()
    });
    let slow = model.update(cx, |m, cx| m.search(message("main"), cx));
    repo.state().delays.remove(&oid(MAIN_COMMIT));
    search(cx, &model, message("main 1")).await;
    slow.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.search_results().len(), 61);
        assert!(
            m.search_results()
                .iter()
                .all(|c| c.title.contains("main 1"))
        );
        assert!(!m.is_searching());
    });
    assert!(
        all_contain(&seen.borrow(), "main 1"),
        "the older filter's page never rendered"
    );
}

#[gpui::test]
async fn a_search_chunk_landing_after_a_newer_search_starts_is_dropped(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    repo.state().search_gate = Some(Arc::clone(&held));
    let seen = record(cx, &model, BrowserEvent::SearchChanged, |m| {
        m.search_results()
            .iter()
            .map(|c| c.title.clone())
            .collect::<Vec<_>>()
    });
    let older = model.update(cx, |m, cx| m.search(message("main"), cx));
    arrived(cx, &held).await;
    search(cx, &model, message("main 1")).await;
    held.open();
    older.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.search_filter(), Some(&message("main 1")));
        assert_eq!(m.search_results().len(), 61);
        assert!(m.search_complete());
        assert!(!m.is_searching());
    });
    assert!(
        all_contain(&seen.borrow(), "main 1"),
        "the older filter's last chunk never rendered"
    );
}

#[gpui::test]
async fn a_superseded_search_walks_only_its_first_small_chunk(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    {
        let mut state = repo.state();
        state
            .commits
            .insert(oid(MAIN_COMMIT), touching(2_000, "filler"));
        state.search_gate = Some(Arc::clone(&held));
    }
    let older = model.update(cx, |m, cx| m.search(message("no such commit"), cx));
    arrived(cx, &held).await;
    search(cx, &model, message("filler")).await;
    held.open();
    older.await;
    cx.run_until_parked();
    assert_eq!(
        repo.state().search_max_scans,
        [SEARCH_FIRST_SCAN_CHUNK, SEARCH_FIRST_SCAN_CHUNK],
        "the replaced query walked one small chunk; the newer one filled its page in one"
    );
    model.read_with(cx, |m, _| {
        assert_eq!(m.search_filter(), Some(&message("filler")));
        assert_eq!(m.search_results().len(), HISTORY_PAGE_SIZE);
    });
}

#[gpui::test]
async fn search_chunks_double_up_to_the_cap(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state()
        .commits
        .insert(oid(MAIN_COMMIT), touching(16_000, "filler"));
    search(cx, &model, message("no such commit")).await;
    assert_eq!(
        repo.state().search_max_scans,
        [500, 1_000, 2_000, 4_000, 8_000, 10_000]
    );
    model.read_with(cx, |m, _| {
        assert!(m.search_complete());
        assert!(m.search_results().is_empty());
    });
}

#[gpui::test]
async fn a_search_typed_before_history_has_an_anchor_runs_once_it_does(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let held = gate();
    repo.state().refs_gate = Some(Arc::clone(&held));
    let repository = Arc::new(repo.clone());
    let attached = model.update(cx, |m, cx| m.attach(repository, cx));
    arrived(cx, &held).await;
    search(cx, &model, message("main 1")).await;
    assert!(
        model.read_with(cx, |m, _| m.search_filter().is_none()),
        "no anchor yet, so nothing is searched"
    );
    held.open();
    attached.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.search_filter(), Some(&message("main 1")));
        assert_eq!(m.search_results().len(), 61);
        assert!(m.search_complete());
        assert!(!m.is_searching());
    });
}

#[gpui::test]
async fn a_search_cleared_before_history_has_an_anchor_never_runs(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    let held = gate();
    repo.state().refs_gate = Some(Arc::clone(&held));
    let repository = Arc::new(repo.clone());
    let attached = model.update(cx, |m, cx| m.attach(repository, cx));
    arrived(cx, &held).await;
    search(cx, &model, message("main 1")).await;
    search(cx, &model, CommitFilter::default()).await;
    held.open();
    attached.await;
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| m.search_filter().is_none()));
    assert_eq!(repo.state().search_calls, 0);
}

#[gpui::test]
async fn a_search_under_a_path_scope_matches_only_commits_touching_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let all = repo.state().commits[&oid(MAIN_COMMIT)].clone();
    let touched = vec![all[3].clone(), all[60].clone(), all[95].clone()];
    scope(&repo, "src/a.rs", touched.clone());
    show_path(cx, &model, "src/a.rs").await;
    search(cx, &model, message("main 9")).await;
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.search_filter().and_then(|f| f.path.as_deref()),
            Some("src/a.rs"),
            "the scope rides along with the search"
        );
        let found: Vec<Oid> = m.search_results().iter().map(|c| c.oid).collect();
        assert_eq!(found, [all[95].oid], "only main 95 is both");
        assert_eq!(
            *m.history_scope(),
            HistoryScope::Path("src/a.rs".to_owned())
        );
    });
    model.update(cx, rc_git_kit::browser::BrowserModel::clear_history_scope);
    search(cx, &model, message("main 9")).await;
    model.read_with(cx, |m, _| {
        assert!(m.search_filter().and_then(|f| f.path.clone()).is_none());
        assert_eq!(m.search_results().len(), 11, "main 9 and main 90-99");
    });
}

#[gpui::test]
async fn ref_switch_and_empty_filter_clear_the_search(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    search(cx, &model, message("main")).await;
    assert!(model.read_with(cx, |m, _| m.search_filter().is_some()));
    let seen = record(cx, &model, BrowserEvent::SearchChanged, |m| {
        m.search_filter().is_none()
    });
    select(cx, &model, "feature").await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert!(m.search_filter().is_none());
        assert!(m.search_results().is_empty());
    });
    assert_eq!(
        seen.borrow().last().copied(),
        Some(true),
        "the view hears the drop, so it can clear its query"
    );
    search(cx, &model, message("feature")).await;
    assert!(model.read_with(cx, |m, _| m.search_filter().is_some()));
    search(cx, &model, CommitFilter::default()).await;
    assert!(
        model.read_with(cx, |m, _| m.search_filter().is_none()),
        "an empty filter clears instead of matching all"
    );
}

#[gpui::test]
async fn path_scope_error_surfaces(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let events = events(cx, &model);
    show_path(cx, &model, "unknown").await;
    cx.run_until_parked();
    assert!(errors(&events)[0].contains("read object header"));
}

#[gpui::test]
async fn path_scope_failure_keeps_shown_commits_and_stops_paging(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    scope(&repo, "big", touching(150, "touch"));
    show_path(cx, &model, "big").await;
    assert_eq!(history_len(cx, &model), 100);
    repo.state().path_history_fail_from = Some(100);
    let events = events(cx, &model);
    more(cx, &model).await;
    model.read_with(cx, |m, _| {
        assert_eq!(m.history().len(), 100, "the commits already shown stay");
        assert!(m.history_error().is_some(), "a failed walk stops paging");
        assert!(!m.history_complete(), "a failure is not the end of history");
        assert!(!m.is_loading_history());
    });
    let calls = repo.state().path_history_calls;
    more(cx, &model).await;
    cx.run_until_parked();
    assert_eq!(repo.state().path_history_calls, calls, "no re-walk");
    assert_eq!(errors(&events).len(), 1, "one alert, not one per scroll");
}

#[gpui::test]
async fn history_is_cached_per_tip_across_reselection(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 100);
    select(cx, &model, "feature").await;
    assert_eq!(history_len(cx, &model), 0, "another tip starts fresh");
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 1);
    assert!(model.read_with(cx, |m, _| m.history_complete()));
    let calls = repo.state().log_calls;
    select(cx, &model, "main").await;
    assert_eq!(history_len(cx, &model), 100);
    assert_eq!(
        repo.state().log_calls,
        calls,
        "cached pages come back without a walk"
    );
}

#[gpui::test]
async fn history_errors_surface(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().commits.remove(&oid(MAIN_COMMIT));
    let events = events(cx, &model);
    more(cx, &model).await;
    cx.run_until_parked();
    assert!(errors(&events)[0].contains("read object header"));
    assert!(!model.read_with(cx, |m, _| m.is_loading_history()));
}

#[gpui::test]
async fn a_failed_history_page_is_not_retried_until_asked(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().commits.remove(&oid(MAIN_COMMIT));
    let events = events(cx, &model);
    more(cx, &model).await;
    more(cx, &model).await;
    cx.run_until_parked();
    assert_eq!(repo.state().log_calls, 1, "a scroll does not re-walk");
    assert_eq!(errors(&events).len(), 1, "one alert, not one per scroll");
    model.read_with(cx, |m, _| {
        assert!(m.history_error().unwrap().contains("read object header"));
        assert!(!m.history_complete());
    });
    assert!(
        count(&events, &BrowserEvent::HistoryChanged) >= 1,
        "the failure renders"
    );

    repo.state().commits.insert(
        oid(MAIN_COMMIT),
        vec![commit_summary(oid(MAIN_COMMIT), &[], "main 0")],
    );
    model
        .update(cx, rc_git_kit::browser::BrowserModel::retry_history)
        .await;
    assert_eq!(repo.state().log_calls, 2, "retry walks again");
    model.read_with(cx, |m, _| {
        assert!(m.history_error().is_none());
        assert_eq!(m.history().len(), 1);
    });
}

#[gpui::test]
async fn search_errors_surface(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().commits.remove(&oid(MAIN_COMMIT));
    let events = events(cx, &model);
    let failed_on_render = record(cx, &model, BrowserEvent::SearchChanged, |m| {
        m.search_error().is_some()
    });
    search(cx, &model, message("x")).await;
    cx.run_until_parked();
    assert!(errors(&events)[0].contains("read object header"));
    model.read_with(cx, |m, _| {
        assert!(!m.is_searching());
        assert!(!m.search_complete());
    });
    assert!(
        failed_on_render.borrow().contains(&true),
        "the search event fired after the failure"
    );
}

#[gpui::test]
async fn a_failed_search_is_not_retried_until_the_filter_changes(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().commits.remove(&oid(MAIN_COMMIT));
    let events = events(cx, &model);
    search(cx, &model, message("main")).await;
    model
        .update(
            cx,
            rc_git_kit::browser::BrowserModel::load_more_search_results,
        )
        .await;
    cx.run_until_parked();
    assert_eq!(repo.state().search_calls, 1, "no automatic retry");
    assert_eq!(errors(&events).len(), 1, "one alert, not one per render");
    assert!(model.read_with(cx, |m, _| m.search_error().is_some()));
    search(cx, &model, message("other")).await;
    assert_eq!(repo.state().search_calls, 2, "a new filter searches again");
    model.update(cx, rc_git_kit::browser::BrowserModel::clear_search);
    assert!(model.read_with(cx, |m, _| m.search_error().is_none()));
}

#[gpui::test]
async fn browsing_a_commit_detaches_the_selection_and_keeps_history(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    more(cx, &model).await;
    let commit = commit_summary(oid(FEATURE_COMMIT), &[], "feature 0");
    let browsed = commit.clone();
    model.update(cx, |m, cx| m.browse_commit(browsed, cx)).await;
    model.read_with(cx, |m, _| {
        assert!(m.selected_ref().is_none());
        assert_eq!(m.browsed_commit(), Some(&commit));
        assert_eq!(m.root_tree(), Some(oid(FEATURE_TREE)));
        assert_eq!(
            m.history().len(),
            100,
            "the history pane keeps showing the ref's walk"
        );
    });
    select(cx, &model, "main").await;
    model.read_with(cx, |m, _| {
        assert!(m.browsed_commit().is_none());
        assert_eq!(m.root_tree(), Some(oid(MAIN_TREE)));
    });
}

#[gpui::test]
async fn refresh_keeps_the_selection_when_its_target_is_unchanged(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let events = events(cx, &model);
    repo.state()
        .listing
        .refs
        .push(branch("v2", oid(MAIN_COMMIT), RefKind::Tag));
    refresh(cx, &model).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.refs().refs.len(), 3);
        assert_eq!(m.selected_ref().unwrap().short_name, "main");
    });
    assert_eq!(
        count(&events, &BrowserEvent::RootChanged),
        0,
        "an unchanged target must not reload the tree"
    );
}

#[gpui::test]
async fn refresh_reloads_when_the_selected_ref_moved(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let moved_tree = oid(0x33);
    {
        let mut state = repo.state();
        state.listing.refs[1] = branch("main", oid(FEATURE_COMMIT), RefKind::LocalBranch);
        state.tree_of.insert(oid(FEATURE_COMMIT), moved_tree);
        state.trees.insert(moved_tree, Vec::new());
    }
    let events = events(cx, &model);
    refresh(cx, &model).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.selected_ref().unwrap().target, oid(FEATURE_COMMIT));
        assert_eq!(m.root_tree(), Some(moved_tree));
        assert!(m.history().is_empty(), "history follows the new tip");
    });
    assert_eq!(count(&events, &BrowserEvent::RootChanged), 1);
}

#[gpui::test]
async fn refresh_falls_back_when_the_selected_ref_disappeared(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    {
        let mut state = repo.state();
        state.listing.refs.retain(|r| r.short_name != "main");
        state.head = head("refs/heads/feature", Some(oid(FEATURE_COMMIT)));
    }
    refresh(cx, &model).await;
    model.read_with(cx, |m, _| {
        assert_eq!(m.selected_ref().unwrap().short_name, "feature");
        assert_eq!(m.root_tree(), Some(oid(FEATURE_TREE)));
    });
}

#[gpui::test]
async fn refresh_with_every_ref_gone_empties_the_browser(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    more(cx, &model).await;
    assert_eq!(history_len(cx, &model), 100);
    {
        let mut state = repo.state();
        state.listing = RefListing::default();
        state.head = Head {
            ref_name: None,
            target: None,
            exact_ref_name: true,
        };
    }
    let events = events(cx, &model);
    refresh(cx, &model).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert!(m.selected_ref().is_none());
        assert!(
            m.root_tree().is_none(),
            "the deleted ref's tree must not stay"
        );
        assert!(m.history().is_empty(), "nor its history");
    });
    assert_eq!(count(&events, &BrowserEvent::RootChanged), 1);
}

#[gpui::test]
async fn refresh_keeps_a_browsed_commit(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let commit = commit_summary(oid(FEATURE_COMMIT), &[], "feature 0");
    let browsed = commit.clone();
    model.update(cx, |m, cx| m.browse_commit(browsed, cx)).await;
    repo.state()
        .listing
        .refs
        .push(branch("v2", oid(MAIN_COMMIT), RefKind::Tag));
    let events = events(cx, &model);
    refresh(cx, &model).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.refs().refs.len(), 3);
        assert!(m.selected_ref().is_none());
        assert_eq!(
            m.browsed_commit(),
            Some(&commit),
            "a fetch must not leave the browsed commit"
        );
        assert_eq!(m.root_tree(), Some(oid(FEATURE_TREE)));
    });
    assert_eq!(count(&events, &BrowserEvent::RootChanged), 0);
}

#[gpui::test]
async fn refresh_failure_keeps_the_last_good_listing_quietly(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().fail_next_refs = true;
    let events = events(cx, &model);
    refresh(cx, &model).await;
    cx.run_until_parked();
    assert!(
        errors(&events).is_empty(),
        "transient lock errors must not alert"
    );
    assert_eq!(
        count(&events, &BrowserEvent::RefsChanged),
        1,
        "the views re-render to show the stale state"
    );
    model.read_with(cx, |m, _| {
        assert!(m.refs_stale(), "the kept listing must not read as current");
        assert_eq!(m.refs().refs.len(), 2);
        assert_eq!(m.selected_ref().unwrap().short_name, "main");
    });

    repo.state()
        .listing
        .refs
        .push(branch("v2", oid(MAIN_COMMIT), RefKind::Tag));
    refresh(cx, &model).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert!(!m.refs_stale(), "a successful read clears the stale state");
        assert_eq!(m.refs().refs.len(), 3);
        assert_eq!(m.selected_ref().unwrap().short_name, "main");
    });
    assert!(errors(&events).is_empty());
}

#[gpui::test]
async fn a_refresh_in_flight_when_another_repository_attaches_is_dropped(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    repo.state().refs_gate = Some(Arc::clone(&held));
    let refreshing = model.update(cx, rc_git_kit::browser::BrowserModel::refresh_refs);
    arrived(cx, &held).await;

    let other = FakeRepository::new();
    {
        let mut state = other.state();
        state.listing = RefListing {
            refs: vec![branch("other", oid(0x51), RefKind::LocalBranch)],
            broken: Vec::new(),
        };
        state.head = head("refs/heads/other", Some(oid(0x51)));
        state.tree_of.insert(oid(0x51), oid(0x52));
        state.trees.insert(oid(0x52), Vec::new());
    }
    attach(cx, &model, &other).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.selected_ref().unwrap().short_name.clone()),
        "other"
    );
    cx.run_until_parked();
    let events = events(cx, &model);
    held.open();
    refreshing.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        let names: Vec<&str> = m
            .refs()
            .refs
            .iter()
            .map(|r| r.short_name.as_str())
            .collect();
        assert_eq!(names, ["other"], "the old listing never lands");
        assert_eq!(m.head().unwrap().target, Some(oid(0x51)));
        assert_eq!(m.root_tree(), Some(oid(0x52)));
    });
    assert_eq!(
        count(&events, &BrowserEvent::RefsChanged),
        0,
        "the dropped refresh re-renders nothing"
    );
}

#[gpui::test]
async fn a_burst_of_refreshes_costs_two_listings(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    {
        let mut state = repo.state();
        state.refs_calls = 0;
        state.refs_delay = Duration::from_millis(300);
    }
    let slow = model.update(cx, rc_git_kit::browser::BrowserModel::refresh_refs);
    let watched = repo.clone();
    until(cx, move || watched.state().refs_calls >= 1).await;
    {
        let mut state = repo.state();
        state
            .listing
            .refs
            .push(branch("v2", oid(MAIN_COMMIT), RefKind::Tag));
        state.refs_delay = Duration::ZERO;
    }
    let burst: Vec<_> = (0..5)
        .map(|_| model.update(cx, rc_git_kit::browser::BrowserModel::refresh_refs))
        .collect();
    slow.await;
    for refresh in burst {
        refresh.await;
    }
    assert_eq!(
        repo.state().refs_calls,
        2,
        "the burst joins one more listing"
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.refs().refs.len()),
        3,
        "the last listing read is the latest"
    );
}
