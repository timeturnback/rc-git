//! Port of `BrowserLocationTests.swift`: what the model reports as its location, how it
//! returns to one, and how restores share their queue with ref refreshes.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{Entity, TestAppContext};
use rc_git_core::{EntryKind, Head, Oid, RefKind, RefListing, WorktreeListing, WorktreeStatus};
use rc_git_kit::back_forward::{BrowsedTree, BrowserLocation, HistoryScope};
use rc_git_kit::browser::{
    BrowserEvent, BrowserModel, CommitDetailState, FileState, WorktreeFileOpen,
};
use rc_git_kit::fake::{FakeRepository, branch, commit_summary, oid, text_blob};

use crate::support::{
    arrived, attach, count, disk, errors, events, file_state, gate, model, record, text, tree,
};

const MAIN_COMMIT: u8 = 0x11;
const MAIN_TREE: u8 = 0x12;
const FEATURE_COMMIT: u8 = 0x21;
const FEATURE_TREE: u8 = 0x22;
const OLDER: u8 = 0x31;
const SRC_TREE: u8 = 0x30;
const README: u8 = 0x40;
const SOURCE: u8 = 0x41;
const MAIN: &str = "refs/heads/main";
const FEATURE: &str = "refs/heads/feature";

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
        state.head = Head {
            ref_name: Some(MAIN.to_owned()),
            target: Some(oid(MAIN_COMMIT)),
            exact_ref_name: true,
        };
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.tree_of.insert(oid(FEATURE_COMMIT), oid(FEATURE_TREE));
        state.tree_of.insert(oid(OLDER), oid(MAIN_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![
                tree("src", oid(SRC_TREE), EntryKind::Tree),
                tree("README.md", oid(README), EntryKind::Blob),
            ],
        );
        state.trees.insert(
            oid(FEATURE_TREE),
            vec![tree("README.md", oid(README), EntryKind::Blob)],
        );
        state.trees.insert(
            oid(SRC_TREE),
            vec![tree("main.rs", oid(SOURCE), EntryKind::Blob)],
        );
        state.blobs.insert(oid(README), text_blob("# hi\n"));
        state.blobs.insert(oid(SOURCE), text_blob("fn main() {}\n"));
        state.commits.insert(
            oid(MAIN_COMMIT),
            vec![
                commit_summary(oid(MAIN_COMMIT), &[oid(OLDER)], "main"),
                commit_summary(oid(OLDER), &[], "older"),
            ],
        );
        state.commits.insert(
            oid(FEATURE_COMMIT),
            vec![commit_summary(oid(FEATURE_COMMIT), &[], "feature")],
        );
        state.tree_diffs.insert(oid(MAIN_COMMIT), Vec::new());
        state.tree_diffs.insert(oid(OLDER), Vec::new());
        state.path_histories.insert(
            (oid(MAIN_COMMIT), "README.md".to_owned()),
            vec![commit_summary(oid(OLDER), &[], "older")],
        );
    }
    repo
}

fn at(tree: BrowsedTree) -> BrowserLocation {
    BrowserLocation {
        tree,
        ..BrowserLocation::default()
    }
}

fn at_file(tree: BrowsedTree, path: &str) -> BrowserLocation {
    BrowserLocation {
        file_path: Some(path.to_owned()),
        ..at(tree)
    }
}

fn reference(name: &str) -> BrowsedTree {
    BrowsedTree::Ref(name.to_owned())
}

fn location(cx: &mut TestAppContext, model: &Entity<BrowserModel>) -> BrowserLocation {
    model.read_with(cx, |m, _| m.location())
}

async fn open(cx: &mut TestAppContext, model: &Entity<BrowserModel>, path: &str) {
    model.update(cx, |m, cx| m.open_path(path, None, cx)).await;
}

async fn select_feature(cx: &mut TestAppContext, model: &Entity<BrowserModel>) {
    let feature = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(feature, cx)).await;
}

async fn restore(cx: &mut TestAppContext, model: &Entity<BrowserModel>, target: BrowserLocation) {
    model.update(cx, |m, cx| m.restore(target, cx)).await;
}

async fn scope_and_detail(cx: &mut TestAppContext, model: &Entity<BrowserModel>) {
    model
        .update(cx, |m, cx| m.show_path_history("README.md", true, cx))
        .await;
    model
        .update(cx, |m, cx| {
            m.load_commit_changes(commit_summary(oid(OLDER), &[], "older"), cx)
        })
        .await;
}

fn on_disk(repo: &FakeRepository) {
    let mut state = repo.state();
    state.work_dir = Some(PathBuf::from("/work"));
    state
        .worktree_contents
        .insert("README.md".to_owned(), text_blob("# on disk\n"));
}

fn opened_path(cx: &mut TestAppContext, model: &Entity<BrowserModel>) -> Option<String> {
    model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned))
}

#[gpui::test]
async fn location_names_the_tree_file_scope_and_commit(cx: &mut TestAppContext) {
    let model = model(cx);
    assert_eq!(
        location(cx, &model),
        BrowserLocation::default(),
        "nothing is open"
    );
    attach(cx, &model, &repo()).await;
    assert_eq!(location(cx, &model), at(reference(MAIN)));
    open(cx, &model, "src/main.rs").await;
    scope_and_detail(cx, &model).await;
    assert_eq!(
        location(cx, &model),
        BrowserLocation {
            tree: reference(MAIN),
            file_path: Some("src/main.rs".to_owned()),
            file_path_is_exact: true,
            history_scope: HistoryScope::Path("README.md".to_owned()),
            detail_commit: Some(oid(OLDER)),
        }
    );
}

#[gpui::test]
async fn a_checkout_opens_on_the_working_tree_and_returns_to_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo);
    repo.state().worktree_listings.insert(
        String::new(),
        WorktreeListing {
            entries: vec![disk(
                "README.md",
                EntryKind::Blob,
                WorktreeStatus::Modified,
                Some(oid(README)),
            )],
            ..WorktreeListing::default()
        },
    );
    attach(cx, &model, &repo).await;
    assert_eq!(location(cx, &model), at(BrowsedTree::WorkingTree));
    open(cx, &model, "README.md").await;
    let on_disk = location(cx, &model);
    select_feature(cx, &model).await;
    restore(cx, &model, on_disk.clone()).await;
    assert!(model.read_with(cx, |m, _| m.browsing_working_tree()));
    assert_eq!(location(cx, &model), on_disk);
    let (_, document) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(document.text, "# on disk\n");
}

#[gpui::test]
async fn every_move_reports_once(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let events = events(cx, &model);
    let reports = |events: &_| count(events, &BrowserEvent::LocationChanged);
    open(cx, &model, "README.md").await;
    cx.run_until_parked();
    assert_eq!(reports(&events), 1);
    open(cx, &model, "README.md").await;
    cx.run_until_parked();
    assert_eq!(reports(&events), 1, "the same file again is not a move");
    model
        .update(cx, |m, cx| m.show_path_history("README.md", true, cx))
        .await;
    cx.run_until_parked();
    assert_eq!(reports(&events), 2);
    model
        .update(cx, |m, cx| {
            m.load_commit_changes(commit_summary(oid(OLDER), &[], "older"), cx)
        })
        .await;
    cx.run_until_parked();
    assert_eq!(reports(&events), 3);
    model.update(cx, BrowserModel::close_commit_detail);
    cx.run_until_parked();
    assert_eq!(reports(&events), 4);
    model.update(cx, BrowserModel::clear_history_scope);
    cx.run_until_parked();
    assert_eq!(reports(&events), 5);
    model.update(cx, BrowserModel::refresh_refs).await;
    cx.run_until_parked();
    assert_eq!(
        reports(&events),
        5,
        "a refresh that moved nothing is not a move"
    );
}

#[gpui::test]
async fn a_ref_switch_reports_where_it_ended_and_not_the_step_between(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open(cx, &model, "README.md").await;
    model
        .update(cx, |m, cx| {
            m.load_commit_changes(commit_summary(oid(OLDER), &[], "older"), cx)
        })
        .await;
    let reported = record(
        cx,
        &model,
        BrowserEvent::LocationChanged,
        BrowserModel::location,
    );
    select_feature(cx, &model).await;
    cx.run_until_parked();
    assert_eq!(
        *reported.borrow(),
        [at(reference(FEATURE))],
        "never the new ref with the old file still open"
    );
}

#[gpui::test]
async fn restore_reopens_an_earlier_file_of_the_same_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    let at_readme = location(cx, &model);
    open(cx, &model, "src/main.rs").await;
    let reads = repo.state().tree_entry_calls;
    restore(cx, &model, at_readme.clone()).await;
    assert_eq!(location(cx, &model), at_readme);
    let (entry, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.oid, Some(oid(README)));
    assert_eq!(
        repo.state().tree_entry_calls,
        reads,
        "the tree did not change, so none is read"
    );
}

#[gpui::test]
async fn restore_closes_a_file_the_location_did_not_have(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let no_file = location(cx, &model);
    open(cx, &model, "README.md").await;
    let events = events(cx, &model);
    restore(cx, &model, no_file).await;
    cx.run_until_parked();
    assert_eq!(opened_path(cx, &model), None);
    assert_eq!(file_state(cx, &model), FileState::Empty);
    assert_eq!(count(&events, &BrowserEvent::FileChanged), 1);
}

#[gpui::test]
async fn restore_switches_the_tree_before_reopening_its_file(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open(cx, &model, "src/main.rs").await;
    let on_main = location(cx, &model);
    select_feature(cx, &model).await;
    restore(cx, &model, on_main.clone()).await;
    model.read_with(cx, |m, _| {
        assert_eq!(m.selected_ref().unwrap().full_name, MAIN);
        assert_eq!(m.root_tree(), Some(oid(MAIN_TREE)));
        assert_eq!(m.location(), on_main);
    });
    let (entry, _) = text(&file_state(cx, &model)).unwrap();
    assert_eq!(entry.oid, Some(oid(SOURCE)));
}

#[gpui::test]
async fn restore_brings_back_the_scope_and_the_commit_detail(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let plain = location(cx, &model);
    scope_and_detail(cx, &model).await;
    let scoped = location(cx, &model);

    restore(cx, &model, plain).await;
    model.read_with(cx, |m, _| {
        assert_eq!(*m.history_scope(), HistoryScope::All);
        assert!(m.diff_commit().is_none());
        assert_eq!(*m.commit_detail(), CommitDetailState::Empty);
    });

    restore(cx, &model, scoped).await;
    model.read_with(cx, |m, _| {
        assert_eq!(
            *m.history_scope(),
            HistoryScope::Path("README.md".to_owned())
        );
        assert_eq!(m.diff_commit().map(|c| c.oid), Some(oid(OLDER)));
        let shown: Vec<Oid> = m.history().iter().map(|c| c.oid).collect();
        assert_eq!(shown, [oid(OLDER)], "the cached page shows at once");
    });
}

#[gpui::test]
async fn restore_of_a_browsed_commit_keeps_the_history_it_was_picked_from(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    model
        .update(cx, |m, cx| {
            m.browse_commit(commit_summary(oid(OLDER), &[], "older"), cx)
        })
        .await;
    let at_commit = location(cx, &model);
    assert_eq!(
        at_commit.tree,
        BrowsedTree::Commit {
            commit: oid(OLDER),
            history_tip: Some(oid(MAIN_COMMIT))
        }
    );
    select_feature(cx, &model).await;
    restore(cx, &model, at_commit.clone()).await;
    model.read_with(cx, |m, _| {
        assert_eq!(m.browsed_commit().map(|c| c.oid), Some(oid(OLDER)));
        assert!(m.selected_ref().is_none());
        assert_eq!(m.location(), at_commit);
    });
    model.update(cx, BrowserModel::load_more_history).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.history().first().map(|c| c.oid)),
        Some(oid(MAIN_COMMIT)),
        "main's history, not feature's"
    );
}

#[gpui::test]
async fn restore_of_a_deleted_ref_reports_and_changes_nothing(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let before = location(cx, &model);
    let events = events(cx, &model);
    restore(
        cx,
        &model,
        at_file(reference("refs/heads/gone"), "README.md"),
    )
    .await;
    cx.run_until_parked();
    assert_eq!(
        errors(&events),
        ["cannot return to refs/heads/gone: the ref no longer exists"]
    );
    assert_eq!(location(cx, &model), before);
}

#[gpui::test]
async fn restore_of_a_pruned_commit_reports_and_changes_nothing(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let before = location(cx, &model);
    let events = events(cx, &model);
    let pruned = at(BrowsedTree::Commit {
        commit: oid(0x99),
        history_tip: Some(oid(MAIN_COMMIT)),
    });
    restore(cx, &model, pruned).await;
    cx.run_until_parked();
    assert!(errors(&events)[0].contains("read commit"));
    assert_eq!(location(cx, &model), before);
}

#[gpui::test]
async fn restore_of_a_file_that_is_gone_leaves_the_pane_empty(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open(cx, &model, "README.md").await;
    restore(cx, &model, at_file(reference(FEATURE), "src/main.rs")).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.selected_ref().unwrap().full_name.clone()),
        FEATURE
    );
    assert_eq!(opened_path(cx, &model), None, "feature has no src/main.rs");
    assert_eq!(file_state(cx, &model), FileState::Empty);
}

#[gpui::test]
async fn restore_of_a_file_gone_from_the_same_tree_closes_the_one_open(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open(cx, &model, "README.md").await;
    restore(cx, &model, at_file(reference(MAIN), "gone.txt")).await;
    assert_eq!(
        opened_path(cx, &model),
        None,
        "not README.md, which the stop never named"
    );
    assert_eq!(file_state(cx, &model), FileState::Empty);
}

#[gpui::test]
async fn restore_of_a_file_gone_from_disk_closes_the_one_open(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo);
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    restore(cx, &model, at_file(BrowsedTree::WorkingTree, "gone.txt")).await;
    assert_eq!(opened_path(cx, &model), None);
    assert_eq!(
        file_state(cx, &model),
        FileState::Empty,
        "not the failed read of a file nobody asked for"
    );
}

#[gpui::test]
async fn restore_of_an_inexact_working_tree_file_reads_nothing(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo);
    let lossy = "caf\u{FFFD}.txt";
    {
        let mut state = repo.state();
        state.worktree_contents.insert(
            lossy.to_owned(),
            text_blob("a file literally named with U+FFFD\n"),
        );
        state.worktree_listings.insert(
            String::new(),
            WorktreeListing {
                entries: vec![
                    disk(
                        "README.md",
                        EntryKind::Blob,
                        WorktreeStatus::Modified,
                        Some(oid(README)),
                    ),
                    disk(lossy, EntryKind::Blob, WorktreeStatus::Untracked, None),
                ],
                ..WorktreeListing::default()
            },
        );
    }
    attach(cx, &model, &repo).await;
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(WorktreeFileOpen::new(lossy, lossy).inexact(), cx)
        })
        .await;
    let inexact = location(cx, &model);
    assert_eq!(inexact.file_path.as_deref(), Some(lossy));
    assert!(!inexact.file_path_is_exact);
    open(cx, &model, "README.md").await;
    let reads = repo.state().worktree_content_calls;
    restore(cx, &model, inexact).await;
    assert_eq!(
        repo.state().worktree_content_calls,
        reads,
        "the stop's path names no one file"
    );
    assert_eq!(opened_path(cx, &model), None);
    assert_eq!(file_state(cx, &model), FileState::Empty);
}

#[gpui::test]
async fn restore_reports_where_it_ended_once(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open(cx, &model, "README.md").await;
    scope_and_detail(cx, &model).await;
    let full = location(cx, &model);
    select_feature(cx, &model).await;
    let reported = record(
        cx,
        &model,
        BrowserEvent::LocationChanged,
        BrowserModel::location,
    );
    restore(cx, &model, full.clone()).await;
    cx.run_until_parked();
    assert_eq!(*reported.borrow(), [full]);
}

#[gpui::test]
async fn a_ref_picked_during_a_restore_ends_the_restore(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    let on_main = location(cx, &model);
    select_feature(cx, &model).await;
    let held = gate();
    repo.state()
        .gates
        .insert(oid(MAIN_COMMIT), Arc::clone(&held));
    let slow = model.update(cx, |m, cx| m.restore(on_main, cx));
    arrived(cx, &held).await;
    select_feature(cx, &model).await;
    held.open();
    slow.await;
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.selected_ref().unwrap().full_name.clone()),
        FEATURE
    );
    assert_eq!(
        opened_path(cx, &model),
        None,
        "README.md also exists on feature; it stays shut"
    );
}

#[gpui::test]
async fn a_restore_asked_for_during_another_is_the_one_that_stands(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let plain = location(cx, &model);
    let held = gate();
    repo.state().gates.insert(oid(README), Arc::clone(&held));
    let target = BrowserLocation {
        detail_commit: Some(oid(OLDER)),
        ..at_file(reference(MAIN), "README.md")
    };
    let slow = model.update(cx, |m, cx| m.restore(target, cx));
    arrived(cx, &held).await;
    restore(cx, &model, plain.clone()).await;
    held.open();
    slow.await;
    cx.run_until_parked();
    assert_eq!(location(cx, &model), plain);
}

#[gpui::test]
async fn two_quick_restores_into_another_tree_end_on_the_second(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    let at_readme = location(cx, &model);
    open(cx, &model, "src/main.rs").await;
    let at_source = location(cx, &model);
    select_feature(cx, &model).await;
    let held = gate();
    repo.state()
        .gates
        .insert(oid(MAIN_COMMIT), Arc::clone(&held));
    let reported = record(
        cx,
        &model,
        BrowserEvent::LocationChanged,
        BrowserModel::location,
    );
    let slow = model.update(cx, |m, cx| m.restore(at_source, cx));
    arrived(cx, &held).await;
    restore(cx, &model, at_readme.clone()).await;
    held.open();
    slow.await;
    cx.run_until_parked();
    assert_eq!(
        location(cx, &model),
        at_readme,
        "opened against main's tree, once it had loaded"
    );
    assert_eq!(
        *reported.borrow(),
        [at_readme],
        "one report, where the pair ended"
    );
}

#[gpui::test]
async fn a_restore_ending_during_a_refresh_leaves_no_report_for_later(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    let held = gate();
    repo.state().refs_gate = Some(Arc::clone(&held));
    let refresh = model.update(cx, BrowserModel::refresh_refs);
    arrived(cx, &held).await;
    let restoring = model.update(cx, |m, cx| {
        m.restore(at_file(reference(MAIN), "gone.txt"), cx)
    });
    held.open();
    restoring.await;
    let events = events(cx, &model);
    refresh.await;
    cx.run_until_parked();
    assert_eq!(opened_path(cx, &model), None);
    assert_eq!(
        count(&events, &BrowserEvent::LocationChanged),
        0,
        "a report after the restore returned is a new stop"
    );
}

#[gpui::test]
async fn a_restore_during_a_refresh_of_a_moved_ref_keeps_its_file(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "src/main.rs").await;
    let moved = oid(0x13);
    let held = gate();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![
                branch("feature", oid(FEATURE_COMMIT), RefKind::LocalBranch),
                branch("main", moved, RefKind::LocalBranch),
            ],
            broken: Vec::new(),
        };
        state.tree_of.insert(moved, oid(MAIN_TREE));
        state.gates.insert(moved, Arc::clone(&held));
    }
    let refresh = model.update(cx, BrowserModel::refresh_refs);
    arrived(cx, &held).await;
    let restoring = model.update(cx, |m, cx| {
        m.restore(at_file(reference(MAIN), "README.md"), cx)
    });
    held.open();
    restoring.await;
    let events = events(cx, &model);
    refresh.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.selected_ref().unwrap().target, moved);
        assert_eq!(
            m.opened_file_path(),
            Some("README.md"),
            "the moved ref's root load closes no file"
        );
    });
    assert_eq!(count(&events, &BrowserEvent::LocationChanged), 0);
}

#[gpui::test]
async fn a_refresh_asked_for_during_a_restore_runs_after_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    let on_main = location(cx, &model);
    select_feature(cx, &model).await;
    let restore_gate = gate();
    repo.state()
        .gates
        .insert(oid(MAIN_COMMIT), Arc::clone(&restore_gate));
    let slow = model.update(cx, |m, cx| m.restore(on_main.clone(), cx));
    arrived(cx, &restore_gate).await;
    let refresh_gate = gate();
    {
        let mut state = repo.state();
        state
            .listing
            .refs
            .push(branch("topic", oid(FEATURE_COMMIT), RefKind::LocalBranch));
        state.refs_gate = Some(Arc::clone(&refresh_gate));
    }
    model.update(cx, BrowserModel::refresh_refs).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.refs().refs.len()),
        2,
        "the restore is still opening main's tree"
    );
    restore_gate.open();
    slow.await;
    assert_eq!(location(cx, &model), on_main);
    arrived(cx, &refresh_gate).await;
    refresh_gate.open();
    // Joins the refresh the restore started on its way out, and returns once it lands.
    model.update(cx, BrowserModel::refresh_refs).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.refs().refs.len()),
        3,
        "the refresh ran once the restore was done"
    );
}

#[gpui::test]
async fn a_running_refresh_yields_to_a_restore_waiting_on_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    open(cx, &model, "README.md").await;
    let on_main = location(cx, &model);
    select_feature(cx, &model).await;
    let first = gate();
    repo.state().refs_gate = Some(Arc::clone(&first));
    let refresh = model.update(cx, BrowserModel::refresh_refs);
    arrived(cx, &first).await;
    let restoring = model.update(cx, |m, cx| m.restore(on_main.clone(), cx));
    let second = gate();
    repo.state().refs_gate = Some(Arc::clone(&second));
    model.update(cx, BrowserModel::refresh_refs).await;
    first.open();
    arrived(cx, &second).await;
    cx.run_until_parked();
    assert_eq!(
        location(cx, &model),
        on_main,
        "the restore ran before the listing it held back"
    );
    second.open();
    restoring.await;
    refresh.await;
}
