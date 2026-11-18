//! Port of `BrowserHistoryTests.swift`, `BrowserDiffTests`: the commit detail, one file's
//! diff, revision jumps, and blame for the open file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Entity, TestAppContext};
use rc_git_core::{
    Blame, BlameRange, BlobDiff, ChangeKind, CommitDetail, CommitSummary, DEFAULT_BLOB_LIMIT,
    DiffHunk, DiffLine, DiffLineKind, EntryKind, Head, LineEnd, Oid, RefKind, RefListing,
    TreeChange, WorktreeListing, WorktreeStatus,
};
use rc_git_kit::browser::{
    BlobOpen, BrowserEvent, BrowserModel, CommitDetailState, WorktreeFileOpen, blame_refusal,
};
use rc_git_kit::fake::{FakeRepository, branch, commit_summary, oid, text_blob};
use rc_git_kit::models::{BlameState, FileDiffState, path_refusal, submodule_summary};
use rc_git_kit::path_search::PathIndexEntry;

use crate::support::{arrived, attach, count, disk, errors, events, gate, model, record, tree};

const MAIN_COMMIT: u8 = 0x11;
const MAIN_TREE: u8 = 0x12;
const PARENT: u8 = 0x10;
const OLD_BLOB: u8 = 0x41;
const NEW_BLOB: u8 = 0x42;

fn change(path: &str, old: Option<Oid>, new: Option<Oid>) -> TreeChange {
    TreeChange {
        kind: ChangeKind::Modified,
        path: path.into(),
        source_path: None,
        old,
        new,
        entry: EntryKind::Blob,
        old_entry: old.map(|_| EntryKind::Blob),
    }
}

fn line(kind: DiffLineKind, text: &str) -> DiffLine {
    DiffLine {
        kind,
        text: text.into(),
        eol: LineEnd::Lf,
    }
}

fn repo() -> FakeRepository {
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.listing = RefListing {
            refs: vec![branch("main", oid(MAIN_COMMIT), RefKind::LocalBranch)],
            broken: Vec::new(),
        };
        state.head = Head {
            ref_name: Some("refs/heads/main".to_owned()),
            target: Some(oid(MAIN_COMMIT)),
            exact_ref_name: true,
        };
        state.tree_of.insert(oid(MAIN_COMMIT), oid(MAIN_TREE));
        state.trees.insert(
            oid(MAIN_TREE),
            vec![tree("a.txt", oid(NEW_BLOB), EntryKind::Blob)],
        );
        state.blobs.insert(oid(NEW_BLOB), text_blob("one\n"));
        state.tree_diffs.insert(
            oid(MAIN_COMMIT),
            vec![change("a.txt", Some(oid(OLD_BLOB)), Some(oid(NEW_BLOB)))],
        );
        state.blob_diffs.insert(
            (Some(oid(OLD_BLOB)), Some(oid(NEW_BLOB))),
            BlobDiff::Text {
                hunks: vec![DiffHunk {
                    old_start: 1,
                    old_lines: 1,
                    new_start: 1,
                    new_lines: 1,
                    lines: vec![
                        line(DiffLineKind::Removed, "zero"),
                        line(DiffLineKind::Added, "one"),
                    ],
                }],
            },
        );
        state.blames.insert(
            "a.txt".to_owned(),
            Blame::Ranges(vec![range(oid(PARENT), false)]),
        );
        state
            .summaries
            .insert(oid(PARENT), commit_summary(oid(PARENT), &[], "parent"));
    }
    repo
}

fn range(commit: Oid, boundary: bool) -> BlameRange {
    BlameRange {
        commit,
        start_line: 1,
        line_count: 1,
        boundary,
    }
}

fn main_commit() -> CommitSummary {
    commit_summary(oid(MAIN_COMMIT), &[oid(PARENT)], "main")
}

fn detail(summary: CommitSummary, committer_time: i64) -> CommitDetail {
    CommitDetail {
        summary,
        body: String::new(),
        committer_name: "Test Committer".to_owned(),
        committer_email: "committer@example.com".to_owned(),
        committer_time,
        committer_offset: 0,
        tree: oid(MAIN_TREE),
    }
}

async fn load(cx: &mut TestAppContext, model: &Entity<BrowserModel>, commit: CommitSummary) {
    model
        .update(cx, |m, cx| m.load_commit_changes(commit, cx))
        .await;
}

async fn open_a(cx: &mut TestAppContext, model: &Entity<BrowserModel>, limit: u64) {
    let entry = tree("a.txt", oid(NEW_BLOB), EntryKind::Blob);
    model
        .update(cx, |m, cx| {
            m.open_file(BlobOpen::new(&entry).at("a.txt").limit(limit), cx)
        })
        .await;
}

async fn blame(cx: &mut TestAppContext, model: &Entity<BrowserModel>) -> BlameState {
    model
        .update(cx, rc_git_kit::browser::BrowserModel::request_blame)
        .await;
    model.read_with(cx, |m, _| m.blame().clone())
}

#[gpui::test]
async fn selecting_a_commit_loads_its_changes_and_first_diff(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let events = events(cx, &model);
    load(cx, &model, main_commit()).await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.diff_commit(), Some(&main_commit()));
        assert_eq!(m.commit_changes().len(), 1);
        let FileDiffState::Loaded(change, BlobDiff::Text { hunks }) = m.file_diff() else {
            panic!("expected a loaded diff, got {:?}", m.file_diff());
        };
        assert_eq!(change.path, "a.txt");
        assert_eq!(hunks.len(), 1);
    });
    assert!(
        count(&events, &BrowserEvent::DiffChanged) >= 2,
        "cleared, then loaded"
    );
}

#[gpui::test]
async fn selecting_a_commit_loads_its_detail_and_ref_badges(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let mut expected = detail(main_commit(), 1_704_326_400);
    expected.body = "The body.".to_owned();
    repo.state()
        .details
        .insert(oid(MAIN_COMMIT), expected.clone());
    load(cx, &model, main_commit()).await;
    let CommitDetailState::Loaded { detail, refs } =
        model.read_with(cx, |m, _| m.commit_detail().clone())
    else {
        panic!("expected a loaded detail");
    };
    assert_eq!(detail, expected);
    let names: Vec<String> = refs.into_iter().map(|r| r.short_name).collect();
    assert_eq!(names, ["main"], "the main branch points at this commit");
}

#[gpui::test]
async fn failed_commit_load_publishes_the_failed_detail(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().tree_diffs.remove(&oid(MAIN_COMMIT));
    let seen = record(cx, &model, BrowserEvent::DiffChanged, |m| {
        m.commit_detail().clone()
    });
    let events = events(cx, &model);
    load(cx, &model, main_commit()).await;
    cx.run_until_parked();
    let state = model.read_with(cx, |m, _| m.commit_detail().clone());
    let CommitDetailState::Failed(summary, message) = state.clone() else {
        panic!("expected a failed detail, got {state:?}");
    };
    assert_eq!(summary, main_commit());
    assert!(message.contains("missing"));
    assert_eq!(
        seen.borrow().last(),
        Some(&state),
        "the last render sees the failure, not the loading state"
    );
    assert_eq!(errors(&events), [message]);
}

#[test]
fn only_a_loaded_commit_with_no_changes_says_no_files_changed() {
    let loading = CommitDetailState::Loading(main_commit());
    assert_eq!(loading.empty_diff_placeholder(0), "Loading\u{2026}");
    let failed = CommitDetailState::Failed(main_commit(), "boom".to_owned());
    assert_eq!(failed.empty_diff_placeholder(0), "");
    assert_eq!(CommitDetailState::Empty.empty_diff_placeholder(0), "");
    let loaded = CommitDetailState::Loaded {
        detail: detail(main_commit(), 1_704_326_400),
        refs: Vec::new(),
    };
    assert_eq!(loaded.empty_diff_placeholder(0), "No files changed");
    assert_eq!(loaded.empty_diff_placeholder(2), "Select a file");
}

#[test]
fn every_state_but_empty_names_the_open_commit() {
    let commit = main_commit();
    assert_eq!(CommitDetailState::Empty.summary(), None);
    let loading = CommitDetailState::Loading(commit.clone());
    assert_eq!(loading.summary(), Some(&commit));
    let failed = CommitDetailState::Failed(commit.clone(), "boom".to_owned());
    assert_eq!(failed.summary(), Some(&commit));
    let loaded = CommitDetailState::Loaded {
        detail: detail(commit.clone(), 1_704_326_400),
        refs: Vec::new(),
    };
    assert_eq!(loaded.summary(), Some(&commit));
}

#[gpui::test]
async fn jumping_to_a_parent_shows_its_detail(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().tree_diffs.insert(oid(PARENT), Vec::new());
    load(cx, &model, main_commit()).await;
    model
        .update(cx, |m, cx| m.jump_to_commit(oid(PARENT), cx))
        .await;
    model.read_with(cx, |m, _| {
        assert_eq!(m.diff_commit().map(|c| c.oid), Some(oid(PARENT)));
        let CommitDetailState::Loaded { detail, .. } = m.commit_detail() else {
            panic!("expected a loaded detail");
        };
        assert_eq!(detail.summary.title, "parent");
    });
}

#[gpui::test]
async fn jump_to_revision_resolves_and_shows_detail(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().resolves.insert("abc1".to_owned(), oid(PARENT));
    repo.state().tree_diffs.insert(oid(PARENT), Vec::new());
    assert!(
        model
            .update(cx, |m, cx| m.jump_to_revision("abc1", cx))
            .await
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.diff_commit().map(|c| c.oid)),
        Some(oid(PARENT))
    );
}

#[gpui::test]
async fn an_unresolved_revision_fails_quietly(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let events = events(cx, &model);
    assert!(
        !model
            .update(cx, |m, cx| m.jump_to_revision("2024", cx))
            .await
    );
    cx.run_until_parked();
    assert!(model.read_with(cx, |m, _| m.diff_commit().is_none()));
    assert!(errors(&events).is_empty());
}

#[gpui::test]
async fn a_click_during_a_revision_resolve_wins(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    {
        let mut state = repo.state();
        state.resolves.insert("abc1".to_owned(), oid(PARENT));
        state
            .resolve_gates
            .insert("abc1".to_owned(), Arc::clone(&held));
        state.tree_diffs.insert(oid(PARENT), Vec::new());
    }
    let shown = record(cx, &model, BrowserEvent::DiffChanged, |m| {
        m.diff_commit().map(|c| c.oid)
    });
    let jump = model.update(cx, |m, cx| m.jump_to_revision("abc1", cx));
    arrived(cx, &held).await;
    load(cx, &model, main_commit()).await;
    held.open();
    assert!(!jump.await, "a dropped jump reports no jump");
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.diff_commit().map(|c| c.oid)),
        Some(oid(MAIN_COMMIT)),
        "the later click stands"
    );
    assert!(
        !shown.borrow().contains(&Some(oid(PARENT))),
        "the stale jump never lands"
    );
}

#[gpui::test]
async fn a_click_during_a_commit_read_wins(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let held = gate();
    repo.state()
        .commit_gates
        .insert(oid(PARENT), Arc::clone(&held));
    repo.state().tree_diffs.insert(oid(PARENT), Vec::new());
    let shown = record(cx, &model, BrowserEvent::DiffChanged, |m| {
        m.diff_commit().map(|c| c.oid)
    });
    let jump = model.update(cx, |m, cx| m.jump_to_commit(oid(PARENT), cx));
    arrived(cx, &held).await;
    load(cx, &model, main_commit()).await;
    held.open();
    jump.await;
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.diff_commit().map(|c| c.oid)),
        Some(oid(MAIN_COMMIT))
    );
    assert!(!shown.borrow().contains(&Some(oid(PARENT))));
}

#[gpui::test]
async fn a_later_commit_detail_wins(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    {
        let mut state = repo.state();
        let stale = TreeChange {
            kind: ChangeKind::Added,
            ..change("stale.txt", None, Some(oid(0x43)))
        };
        state.tree_diffs.insert(oid(PARENT), vec![stale]);
        state.delays.insert(oid(PARENT), Duration::from_millis(300));
    }
    let shown = record(cx, &model, BrowserEvent::DiffChanged, |m| {
        m.commit_detail().clone()
    });
    let slow = model.update(cx, |m, cx| {
        m.load_commit_changes(commit_summary(oid(PARENT), &[], "parent"), cx)
    });
    load(cx, &model, main_commit()).await;
    slow.await;
    cx.run_until_parked();
    model.read_with(cx, |m, _| {
        assert_eq!(m.diff_commit().map(|c| c.oid), Some(oid(MAIN_COMMIT)));
        let paths: Vec<String> = m
            .commit_changes()
            .iter()
            .map(|c| c.path.to_string())
            .collect();
        assert_eq!(paths, ["a.txt"]);
    });
    let stale_landed = shown.borrow().iter().any(|state| {
        let CommitDetailState::Loaded { detail, .. } = state else {
            return false;
        };
        detail.summary.oid == oid(PARENT)
    });
    assert!(!stale_landed, "the slow commit's detail never rendered");
}

#[gpui::test]
async fn commit_with_no_changes_shows_empty_detail(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().tree_diffs.insert(oid(0x77), Vec::new());
    load(cx, &model, commit_summary(oid(0x77), &[], "empty")).await;
    model.read_with(cx, |m, _| {
        assert!(m.commit_changes().is_empty());
        assert_eq!(*m.file_diff(), FileDiffState::Empty);
    });
}

#[gpui::test]
async fn diff_errors_surface(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    let events = events(cx, &model);
    load(cx, &model, commit_summary(oid(0x99), &[], "gone")).await;
    cx.run_until_parked();
    assert!(errors(&events)[0].contains("read tree"));
    let broken = change("b.txt", Some(oid(0x51)), Some(oid(0x52)));
    model.update(cx, |m, cx| m.load_file_diff(broken, cx)).await;
    let FileDiffState::Failed(failed, message) = model.read_with(cx, |m, _| m.file_diff().clone())
    else {
        panic!("expected a failure");
    };
    assert_eq!(failed.path, "b.txt");
    assert!(message.contains("read blob"));
}

#[gpui::test]
async fn a_submodule_change_never_reaches_diff_blobs(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let bump = TreeChange {
        entry: EntryKind::Submodule,
        old_entry: Some(EntryKind::Submodule),
        ..change("sub", Some(oid(0x11)), Some(oid(0x22)))
    };
    let events = events(cx, &model);
    let loaded = bump.clone();
    model.update(cx, |m, cx| m.load_file_diff(loaded, cx)).await;
    cx.run_until_parked();
    assert_eq!(repo.state().diff_blobs_calls, 0);
    let FileDiffState::Submodule(shown) = model.read_with(cx, |m, _| m.file_diff().clone()) else {
        panic!("expected a submodule state");
    };
    assert_eq!(shown, bump);
    assert_eq!(count(&events, &BrowserEvent::DiffChanged), 1);
    assert_eq!(
        submodule_summary(&shown),
        format!(
            "Subproject commit {} \u{2192} {}",
            oid(0x11).to_hex(),
            oid(0x22).to_hex()
        )
    );
}

#[gpui::test]
async fn a_file_swapped_with_a_submodule_diffs_only_its_file_side(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let blob = oid(0x53);
    let commit = oid(0x33);
    let hunk = DiffHunk {
        old_start: 1,
        old_lines: 1,
        new_start: 0,
        new_lines: 0,
        lines: Vec::new(),
    };
    {
        let mut state = repo.state();
        state.blob_diffs.insert(
            (Some(blob), None),
            BlobDiff::Text {
                hunks: vec![hunk.clone()],
            },
        );
        state
            .blob_diffs
            .insert((None, Some(blob)), BlobDiff::Text { hunks: Vec::new() });
    }
    let to_submodule = TreeChange {
        entry: EntryKind::Submodule,
        old_entry: Some(EntryKind::Blob),
        ..change("lib", Some(blob), Some(commit))
    };
    let to_file = TreeChange {
        entry: EntryKind::Blob,
        old_entry: Some(EntryKind::Submodule),
        ..change("lib", Some(commit), Some(blob))
    };
    let loaded = to_submodule.clone();
    model.update(cx, |m, cx| m.load_file_diff(loaded, cx)).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.file_diff().clone()),
        FileDiffState::Loaded(to_submodule, BlobDiff::Text { hunks: vec![hunk] })
    );
    let loaded = to_file.clone();
    model.update(cx, |m, cx| m.load_file_diff(loaded, cx)).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.file_diff().clone()),
        FileDiffState::Loaded(to_file, BlobDiff::Text { hunks: Vec::new() })
    );
    assert_eq!(repo.state().diff_blobs_calls, 2);
}

#[gpui::test]
async fn closing_the_commit_detail_empties_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().tree_diffs.insert(oid(PARENT), Vec::new());
    load(cx, &model, main_commit()).await;
    model
        .update(cx, |m, cx| m.jump_to_commit(oid(PARENT), cx))
        .await;
    let events = events(cx, &model);
    model.update(cx, rc_git_kit::browser::BrowserModel::close_commit_detail);
    model.read_with(cx, |m, _| {
        assert!(m.diff_commit().is_none());
        assert!(m.commit_changes().is_empty());
        assert_eq!(*m.commit_detail(), CommitDetailState::Empty);
    });
    assert_eq!(count(&events, &BrowserEvent::DiffChanged), 1);
    model.update(cx, rc_git_kit::browser::BrowserModel::close_commit_detail);
    assert_eq!(
        count(&events, &BrowserEvent::DiffChanged),
        1,
        "closing an empty detail is silent"
    );
}

#[gpui::test]
async fn switching_refs_clears_the_commit_detail(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    load(cx, &model, main_commit()).await;
    assert!(model.read_with(cx, |m, _| m.diff_commit().is_some()));
    let main = model.read_with(cx, |m, _| m.refs().refs[0].clone());
    model.update(cx, |m, cx| m.select(main, cx)).await;
    model.read_with(cx, |m, _| {
        assert!(m.diff_commit().is_none());
        assert!(m.commit_changes().is_empty());
        assert_eq!(*m.commit_detail(), CommitDetailState::Empty);
    });
}

#[gpui::test]
async fn blame_needs_an_open_file_with_a_known_path(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    assert_eq!(
        blame(cx, &model).await,
        BlameState::Failed("no file with a known path is open".to_owned())
    );
    open_a(cx, &model, DEFAULT_BLOB_LIMIT).await;
    let BlameState::Loaded { path, spans, .. } = blame(cx, &model).await else {
        panic!("expected a loaded blame");
    };
    assert_eq!(path, "a.txt");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].commit.title, "parent");
    assert_eq!(spans[0].range.line_count, 1);
    assert_eq!(
        spans[0].older_than, None,
        "a traced range is not older than the floor"
    );
    assert_eq!(repo.state().blame_limits, [DEFAULT_BLOB_LIMIT]);
}

#[gpui::test]
async fn blame_walks_two_years_back_from_the_tip_and_marks_older_lines(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    let tip_time = 1_700_000_000;
    {
        let mut state = repo.state();
        state.details.insert(
            oid(MAIN_COMMIT),
            detail(commit_summary(oid(MAIN_COMMIT), &[], "main"), tip_time),
        );
        state.blames.insert(
            "a.txt".to_owned(),
            Blame::Ranges(vec![range(oid(PARENT), true)]),
        );
    }
    open_a(cx, &model, DEFAULT_BLOB_LIMIT).await;
    let BlameState::Loaded { spans, .. } = blame(cx, &model).await else {
        panic!("expected a loaded blame");
    };
    let floor = tip_time - 730 * 86_400;
    assert_eq!(
        repo.state().blame_floors,
        [Some(floor)],
        "the floor is two years before the tip's commit"
    );
    assert_eq!(spans[0].older_than, Some(floor));
}

#[gpui::test]
async fn blame_uses_the_file_pane_limit_and_declines_a_file_over_it(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    attach(cx, &model, &repo).await;
    repo.state().blames.insert(
        "a.txt".to_owned(),
        Blame::TooLarge {
            size: 40,
            limit: 10,
        },
    );
    open_a(cx, &model, 10).await;
    let BlameState::Failed(message) = blame(cx, &model).await else {
        panic!("expected a failed blame");
    };
    assert!(message.contains("40 bytes, over the 10 byte limit"));
    assert_eq!(repo.state().blame_limits, [10]);
}

fn on_disk(repo: &FakeRepository, path: &str, text: &str) {
    let mut state = repo.state();
    state.work_dir = Some(PathBuf::from("/w"));
    state
        .worktree_contents
        .insert(path.to_owned(), text_blob(text));
}

#[gpui::test]
async fn blame_in_the_working_tree_uses_head(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo, "a.txt", "one\ntwo\n");
    attach(cx, &model, &repo).await;
    assert!(model.read_with(cx, |m, _| m.browsing_working_tree()));
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(
                WorktreeFileOpen::new("a.txt", "a.txt").status(WorktreeStatus::Modified),
                cx,
            )
        })
        .await;
    assert!(model.read_with(cx, |m, _| m.can_blame_opened_file()));
    let BlameState::Loaded { path, spans, title } = blame(cx, &model).await else {
        panic!("expected a loaded blame");
    };
    assert_eq!(path, "a.txt");
    assert_eq!(spans[0].commit.title, "parent");
    assert_eq!(
        repo.state().blame_tips,
        [oid(MAIN_COMMIT)],
        "the working tree blames HEAD's version"
    );
    assert_eq!(title, "Blame \u{2014} a.txt at HEAD");
}

#[test]
fn blame_refusal_names_the_first_unmet_condition_and_none_when_all_hold() {
    let refusal = |open, path: Option<&str>, at, untracked, exact| {
        blame_refusal(open, path, at, untracked, exact)
    };
    let path = Some("a.txt");
    assert_eq!(refusal(true, path, true, false, true), None);
    assert_eq!(
        refusal(false, None, false, false, true).as_deref(),
        Some("no repository is open")
    );
    assert_eq!(
        refusal(true, None, false, false, true).as_deref(),
        Some("no file with a known path is open")
    );
    assert_eq!(
        refusal(true, path, false, true, true).as_deref(),
        Some("nothing is selected to blame at")
    );
    assert_eq!(
        refusal(true, path, true, true, false).as_deref(),
        Some("a.txt is untracked; it has no history to blame")
    );
    assert_eq!(
        refusal(true, path, true, false, false),
        Some(path_refusal("blame", "a.txt"))
    );
}

#[gpui::test]
async fn blame_refuses_an_untracked_file_in_the_working_tree(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo, "new.txt", "fresh\n");
    attach(cx, &model, &repo).await;
    model
        .update(cx, |m, cx| {
            m.open_worktree_file(
                WorktreeFileOpen::new("new.txt", "new.txt").status(WorktreeStatus::Untracked),
                cx,
            )
        })
        .await;
    assert!(
        !model.read_with(cx, |m, _| m.can_blame_opened_file()),
        "Show Blame is disabled for an untracked file"
    );
    assert_eq!(
        blame(cx, &model).await,
        BlameState::Failed("new.txt is untracked; it has no history to blame".to_owned())
    );
    assert!(repo.state().blame_tips.is_empty(), "no walk runs");
}

fn untracked_listing(repo: &FakeRepository) {
    repo.state().worktree_listings.insert(
        String::new(),
        WorktreeListing {
            entries: vec![disk(
                "new.txt",
                EntryKind::Blob,
                WorktreeStatus::Untracked,
                None,
            )],
            ..WorktreeListing::default()
        },
    );
}

#[gpui::test]
async fn blame_refuses_an_untracked_file_opened_by_link(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo, "new.txt", "fresh\n");
    untracked_listing(&repo);
    attach(cx, &model, &repo).await;
    let outcome = model
        .update(cx, |m, cx| m.open_path("new.txt", None, cx))
        .await;
    assert_eq!(outcome, rc_git_kit::browser::OpenPathOutcome::Opened);
    assert!(
        !model.read_with(cx, |m, _| m.can_blame_opened_file()),
        "the link lookup carries the file's status"
    );
}

#[gpui::test]
async fn blame_refuses_an_untracked_file_opened_from_path_search(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    on_disk(&repo, "new.txt", "fresh\n");
    untracked_listing(&repo);
    attach(cx, &model, &repo).await;
    let pick = PathIndexEntry::new("new.txt".to_owned(), None, EntryKind::Blob, false, true);
    model
        .update(cx, |m, cx| m.open_search_result(&pick, cx))
        .await;
    assert_eq!(
        model.read_with(cx, |m, _| m.opened_file_path().map(str::to_owned)),
        Some("new.txt".to_owned())
    );
    assert!(
        !model.read_with(cx, |m, _| m.can_blame_opened_file()),
        "the search pick carries the file's status"
    );
}

#[gpui::test]
async fn blame_of_a_ref_is_not_titled_head(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    assert!(!model.read_with(cx, |m, _| m.browsing_working_tree()));
    open_a(cx, &model, DEFAULT_BLOB_LIMIT).await;
    let BlameState::Loaded { title, .. } = blame(cx, &model).await else {
        panic!("expected a loaded blame");
    };
    assert_eq!(title, "Blame \u{2014} a.txt");
}

#[gpui::test]
async fn a_file_opened_without_a_path_drops_the_previous_path(cx: &mut TestAppContext) {
    let model = model(cx);
    attach(cx, &model, &repo()).await;
    open_a(cx, &model, DEFAULT_BLOB_LIMIT).await;
    assert!(model.read_with(cx, |m, _| m.can_blame_opened_file()));
    let other = tree("b.txt", oid(NEW_BLOB), EntryKind::Blob);
    model
        .update(cx, |m, cx| m.open_file(BlobOpen::new(&other), cx))
        .await;
    model.read_with(cx, |m, _| {
        assert!(
            m.opened_file_path().is_none(),
            "blame must not target a.txt"
        );
        assert!(!m.can_blame_opened_file());
    });
}

#[gpui::test]
async fn blame_with_nothing_selected_is_refused(cx: &mut TestAppContext) {
    let model = model(cx);
    let repo = repo();
    {
        let mut state = repo.state();
        state.listing = RefListing::default();
        state.head = Head {
            ref_name: None,
            target: None,
            exact_ref_name: true,
        };
    }
    attach(cx, &model, &repo).await;
    open_a(cx, &model, DEFAULT_BLOB_LIMIT).await;
    assert!(!model.read_with(cx, |m, _| m.can_blame_opened_file()));
    let BlameState::Failed(message) = blame(cx, &model).await else {
        panic!("expected a failed blame");
    };
    assert!(message.contains("nothing is selected"));
}
