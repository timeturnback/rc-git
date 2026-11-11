//! Ported from `ChangesModelTests.swift`: the change list's scans, ticks, diffs and stashes
//! over the fake repository. Scans and stash reads are what the coalescing saves, so the
//! fake's call counters carry those assertions; races are ordered with `CallGate`s.
#![expect(
    clippy::panic,
    reason = "helpers outside test bodies fail the test on an unexpected state"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::bstr::BString;
use rc_git_core::{
    BlobDiff, DiffHunk, DiffLine, DiffLineKind, EntryKind, Head, LineEnd, StashEntry, StatusEntry,
    StatusReport, WorktreeStatus,
};
use rc_git_guard::actions::ActionError;
use rc_git_kit::change_tree::{self, ChangeGrouping, ChangeNode};
use rc_git_kit::changes::{
    ChangesEvent, ChangesModel, ChangesState, RowTick, StashListState, TickState, TickSummary,
    WorktreeDiffState,
};
use rc_git_kit::fake::{CallGate, FakeRepository, oid};
use rc_git_kit::guard_bridge::{CommitSelection, GuardFailure, HunkStamp};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::{ChangeGroupingStore, SharedSettings};

const HEAD: u8 = 0xAA;
const TREE: u8 = 0xBB;

fn entry(path: &[u8], status: WorktreeStatus, blob: Option<u8>, size: u64) -> StatusEntry {
    StatusEntry {
        path: BString::from(path),
        kind: EntryKind::Blob,
        status,
        oid: blob.map(oid),
        size,
    }
}

fn report(entries: Vec<StatusEntry>, scanned: usize) -> StatusReport {
    StatusReport {
        entries,
        scanned,
        ..StatusReport::default()
    }
}

fn standard_report() -> StatusReport {
    report(
        vec![
            entry(b"a.txt", WorktreeStatus::Modified, Some(0x01), 3),
            entry(b"b.txt", WorktreeStatus::Untracked, None, 2),
            entry(b"gone.txt", WorktreeStatus::Deleted, Some(0x02), 0),
        ],
        5,
    )
}

fn hunk(old: (u32, u32), new: (u32, u32), text: &str) -> DiffHunk {
    DiffHunk {
        old_start: old.0,
        old_lines: old.1,
        new_start: new.0,
        new_lines: new.1,
        lines: vec![DiffLine {
            kind: DiffLineKind::Added,
            text: BString::from(text),
            eol: LineEnd::Lf,
        }],
    }
}

fn first_hunk() -> DiffHunk {
    hunk((1, 3), (1, 3), "first")
}

fn second_hunk() -> DiffHunk {
    hunk((8, 2), (8, 3), "second")
}

fn text(hunks: Vec<DiffHunk>) -> BlobDiff {
    BlobDiff::Text { hunks }
}

fn stamp(hunk: &DiffHunk) -> HunkStamp {
    HunkStamp::from(hunk)
}

fn whole(path: &str) -> CommitSelection {
    CommitSelection::Whole {
        path: path.to_owned(),
    }
}

fn stash(seed: u8, index: usize, message: &str) -> StashEntry {
    StashEntry {
        index,
        oid: oid(seed),
        message: message.to_owned(),
    }
}

fn strings(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

fn settings() -> SharedSettings {
    Arc::new(MemorySettings::new())
}

struct Fixture {
    repo: FakeRepository,
    model: Entity<ChangesModel>,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    cx.executor().allow_parking();
    let repo = FakeRepository::new();
    {
        let mut state = repo.state();
        state.head = Head {
            ref_name: Some("refs/heads/main".to_owned()),
            target: Some(oid(HEAD)),
            exact_ref_name: true,
        };
        state.tree_of.insert(oid(HEAD), oid(TREE));
        state.status_report = standard_report();
    }
    let reading = Arc::new(repo.clone());
    let model =
        cx.new(|_| ChangesModel::new(reading, ChangeGroupingStore::worktree(settings()), false));
    Fixture { repo, model }
}

impl Fixture {
    async fn load_status(&self, cx: &mut TestAppContext) {
        self.model.update(cx, ChangesModel::load_status).await;
    }

    async fn load_stashes(&self, cx: &mut TestAppContext) {
        self.model.update(cx, ChangesModel::load_stashes).await;
    }

    async fn refresh(&self, cx: &mut TestAppContext) {
        self.model.update(cx, ChangesModel::refresh).await;
    }

    async fn set_visible(&self, cx: &mut TestAppContext, visible: bool) {
        self.model
            .update(cx, |m, cx| m.set_visible(visible, cx))
            .await;
    }

    async fn select(&self, cx: &mut TestAppContext, entry: StatusEntry) {
        self.model.update(cx, |m, cx| m.select(entry, cx)).await;
    }

    fn read<R>(&self, cx: &mut TestAppContext, read: impl FnOnce(&ChangesModel) -> R) -> R {
        self.model.read_with(cx, |m, _| read(m))
    }

    fn state(&self, cx: &mut TestAppContext) -> ChangesState {
        self.read(cx, |m| m.status_state().clone())
    }

    fn diff(&self, cx: &mut TestAppContext) -> WorktreeDiffState {
        self.read(cx, |m| m.diff_state().clone())
    }

    fn entries(&self, cx: &mut TestAppContext) -> Vec<StatusEntry> {
        match self.state(cx) {
            ChangesState::Loaded(report) => report.entries,
            other => panic!("expected loaded, got {other:?}"),
        }
    }

    fn status_calls(&self) -> usize {
        self.repo.state().status_calls
    }

    fn stash_calls(&self) -> usize {
        self.repo.state().stash_calls
    }

    fn set_report(&self, report: StatusReport) {
        self.repo.state().status_report = report;
    }

    fn set_ticked(&self, cx: &mut TestAppContext, paths: &[&str], ticked: bool) {
        let paths = strings(paths);
        self.model
            .update(cx, |m, cx| m.set_ticked(&paths, ticked, cx));
    }

    fn toggle_hunk(
        &self,
        cx: &mut TestAppContext,
        path: &str,
        which: HunkStamp,
        all: &[HunkStamp],
    ) {
        self.model
            .update(cx, |m, cx| m.toggle_hunk(path, which, all, cx));
    }

    fn row(&self, cx: &mut TestAppContext, key: &str) -> ChangeNode<StatusEntry> {
        let entries = self.entries(cx);
        let mut pending = change_tree::nodes(&entries, ChangeGrouping::Folders);
        while let Some(node) = pending.pop() {
            if node.key() == key {
                return node;
            }
            pending.extend(node.children);
        }
        panic!("no row {key}");
    }

    fn row_tick(&self, cx: &mut TestAppContext, key: &str) -> RowTick {
        let node = self.row(cx, key);
        self.read(cx, |m| m.row_tick(&node))
    }

    /// Every event the model emits, in order.
    fn events(&self, cx: &mut TestAppContext) -> Rc<RefCell<Vec<ChangesEvent>>> {
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        cx.update(|cx| {
            cx.subscribe(&self.model, move |_, event: &ChangesEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });
        log
    }

    /// Every diff state the pane was told to show, read as each change was announced.
    fn shown_diffs(&self, cx: &mut TestAppContext) -> Rc<RefCell<Vec<WorktreeDiffState>>> {
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        cx.update(|cx| {
            cx.subscribe(&self.model, move |model, event: &ChangesEvent, cx| {
                if *event == ChangesEvent::DiffChanged {
                    sink.borrow_mut().push(model.read(cx).diff_state().clone());
                }
            })
            .detach();
        });
        log
    }
}

fn count(log: &Rc<RefCell<Vec<ChangesEvent>>>, wanted: &ChangesEvent) -> usize {
    log.borrow().iter().filter(|event| *event == wanted).count()
}

#[gpui::test]
async fn inexact_name_stays_out_of_commit_stash_and_discard(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let spelling = "f\u{FFFD}o";
    f.set_report(report(
        vec![
            entry(b"a.txt", WorktreeStatus::Modified, Some(0x01), 3),
            entry(b"f\xFFo", WorktreeStatus::Deleted, Some(0x02), 0),
            entry(spelling.as_bytes(), WorktreeStatus::Untracked, None, 1),
        ],
        4,
    ));
    f.load_status(cx).await;
    assert!(
        !f.read(cx, |m| m.is_actionable(spelling)),
        "the valid twin shares the spelling"
    );
    assert!(!f.read(cx, |m| m.is_ticked(spelling)));
    f.set_ticked(cx, &[spelling], true);
    let one = HunkStamp {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 1,
    };
    f.toggle_hunk(cx, spelling, one, &[one]);
    assert_eq!(f.read(cx, ChangesModel::ticked_paths), ["a.txt"]);
    assert_eq!(f.read(cx, ChangesModel::selections), [whole("a.txt")]);
    assert_eq!(
        f.read(cx, |m| m.discard_refusal(&strings(&["a.txt"]))),
        None
    );
    assert_eq!(
        f.read(cx, |m| m.discard_refusal(&strings(&["a.txt", spelling]))),
        Some("cannot discard 'f\u{FFFD}o': the path is not valid UTF-8".to_owned())
    );
}

#[gpui::test]
async fn recovering_from_a_stale_repository_reloads_both_lists(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.refresh(cx).await;
    let before = (f.status_calls(), f.stash_calls());
    let error = ActionError::StaleRepository { op: "stash drop" };
    let response = f.model.update(cx, |m, cx| m.recover(&error, cx)).await;
    assert!(
        matches!(response, GuardFailure::Stale { .. }),
        "expected a stale notice, got {response:?}"
    );
    assert_eq!(f.status_calls(), before.0 + 1);
    assert_eq!(f.stash_calls(), before.1 + 1);
}

#[gpui::test]
async fn recovering_from_any_other_failure_leaves_the_lists_alone(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.refresh(cx).await;
    let before = (f.status_calls(), f.stash_calls());
    let error = ActionError::Denied {
        reason: "discard: refused".to_owned(),
    };
    let response = f.model.update(cx, |m, cx| m.recover(&error, cx)).await;
    assert_eq!(
        response,
        GuardFailure::Failed {
            message: "discard: refused".to_owned()
        }
    );
    assert_eq!((f.status_calls(), f.stash_calls()), before);
}

#[gpui::test]
async fn loads_status_and_captures_expected_head(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let events = f.events(cx);
    f.load_status(cx).await;
    assert_eq!(f.entries(cx).len(), 3);
    assert_eq!(f.read(cx, ChangesModel::expected_head), Some(oid(HEAD)));
    assert_eq!(f.repo.state().status_trees, [Some(oid(TREE))]);
    assert_eq!(
        count(&events, &ChangesEvent::StatusChanged),
        2,
        "one for loading, one for loaded"
    );
}

#[gpui::test]
async fn a_tick_change_reports_ticks_and_leaves_the_list_alone(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let events = f.events(cx);
    f.set_ticked(cx, &["a.txt"], false);
    assert_eq!(count(&events, &ChangesEvent::TicksChanged), 1);
    assert_eq!(
        count(&events, &ChangesEvent::StatusChanged),
        0,
        "a tick click must not rebuild the list"
    );
    let first = stamp(&hunk((1, 1), (1, 1), "x"));
    let second = stamp(&hunk((5, 1), (5, 1), "y"));
    f.toggle_hunk(cx, "a.txt", first, &[first, second]);
    assert_eq!(count(&events, &ChangesEvent::TicksChanged), 2);
    assert_eq!(count(&events, &ChangesEvent::StatusChanged), 0);
}

#[gpui::test]
async fn an_unborn_branch_lists_every_file_as_untracked(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().head.target = None;
    f.set_report(report(
        vec![
            entry(b"a.txt", WorktreeStatus::Untracked, None, 3),
            entry(b"src/b.txt", WorktreeStatus::Untracked, None, 2),
        ],
        3,
    ));
    f.load_status(cx).await;
    let entries = f.entries(cx);
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry.status == WorktreeStatus::Untracked)
    );
    assert_eq!(f.read(cx, ChangesModel::expected_head), None);
    assert_eq!(f.repo.state().status_trees, [None], "scanned with no tree");
    assert_eq!(
        f.read(cx, ChangesModel::ticked_paths),
        ["a.txt", "src/b.txt"]
    );
}

#[gpui::test]
async fn untracked_hidden_files_stay_out_of_the_list_and_the_selection(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let mut hidden = report(
        vec![
            entry(b".DS_Store", WorktreeStatus::Untracked, None, 6),
            entry(b"assets/.DS_Store", WorktreeStatus::Modified, Some(0x03), 6),
            entry(b"b.txt", WorktreeStatus::Untracked, None, 2),
            entry(b"sub/._b.txt", WorktreeStatus::Untracked, None, 2),
        ],
        9,
    );
    hidden.truncated = true;
    f.set_report(hidden);
    f.load_status(cx).await;
    let ChangesState::Loaded(listed) = f.state(cx) else {
        panic!("expected loaded");
    };
    let paths: Vec<_> = listed.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(
        paths,
        ["assets/.DS_Store", "b.txt"],
        "a tracked hidden file that changed stays listed"
    );
    assert_eq!((listed.scanned, listed.truncated), (9, true));
    assert_eq!(
        f.read(cx, ChangesModel::ticked_paths),
        ["assets/.DS_Store", "b.txt"]
    );

    f.model.update(cx, |m, _| m.set_shows_hidden_files(true));
    f.load_status(cx).await;
    assert_eq!(
        f.read(cx, ChangesModel::ticked_paths),
        [".DS_Store", "assets/.DS_Store", "b.txt", "sub/._b.txt"]
    );
}

#[gpui::test]
async fn a_failed_scan_shows_the_error(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().fail_next_status = true;
    f.load_status(cx).await;
    let ChangesState::Failed(message) = f.state(cx) else {
        panic!("expected failed");
    };
    assert!(message.contains("scan working tree"));
}

#[gpui::test]
async fn a_burst_of_requests_costs_one_rescan(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let gate = Arc::new(CallGate::new());
    f.repo.state().status_gate = Some(Arc::clone(&gate));
    let in_flight = f.model.update(cx, ChangesModel::load_status);
    assert!(gate.wait_until_arrived());
    for _ in 0..20 {
        f.load_status(cx).await;
    }
    gate.open();
    in_flight.await;
    assert_eq!(
        f.status_calls(),
        2,
        "the scan in flight plus one re-run, however many requests arrived"
    );
    assert!(matches!(f.state(cx), ChangesState::Loaded(_)));
    assert!(!f.read(cx, ChangesModel::is_scanning));
}

/// Cancels a scan held in flight and checks what a cancel promises: idle at once, the held
/// result never lands, no re-run, and the next scan is clean.
async fn cancel_held_scan(f: &Fixture, cx: &mut TestAppContext) {
    let gate = Arc::new(CallGate::new());
    f.repo.state().status_gate = Some(Arc::clone(&gate));
    let calls = f.status_calls();
    let in_flight = f.model.update(cx, ChangesModel::load_status);
    assert!(gate.wait_until_arrived());
    f.load_status(cx).await;
    f.model.update(cx, ChangesModel::cancel_scans);
    assert_eq!(
        f.state(cx),
        ChangesState::Idle,
        "the tab reloads on the next switch back"
    );
    gate.open();
    in_flight.await;
    assert_eq!(
        f.state(cx),
        ChangesState::Idle,
        "the cancelled scan's result never lands"
    );
    assert_eq!(
        f.status_calls(),
        calls + 1,
        "a cancel drops the queued re-run"
    );
    assert!(!f.read(cx, ChangesModel::is_scanning), "the loop ended");
    f.load_status(cx).await;
    assert!(
        matches!(f.state(cx), ChangesState::Loaded(_)),
        "the next scan is clean"
    );
    assert_eq!(f.status_calls(), calls + 2);
}

#[gpui::test]
async fn a_cancelled_scan_is_dropped_and_leaves_the_list_idle(cx: &mut TestAppContext) {
    let f = fixture(cx);
    cancel_held_scan(&f, cx).await;
}

#[gpui::test]
async fn the_selection_survives_a_rescan_in_flight(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let before = f.state(cx);
    let gate = Arc::new(CallGate::new());
    f.repo.state().status_gate = Some(Arc::clone(&gate));
    let rescan = f.model.update(cx, ChangesModel::load_status);
    assert!(gate.wait_until_arrived());
    assert_eq!(
        f.state(cx),
        before,
        "the list keeps the last report while it rescans"
    );
    assert_eq!(
        f.read(cx, ChangesModel::selections),
        [whole("a.txt"), whole("b.txt"), whole("gone.txt")]
    );
    assert_eq!(
        f.read(cx, ChangesModel::ticked_paths),
        ["a.txt", "b.txt", "gone.txt"]
    );
    gate.open();
    rescan.await;
}

#[gpui::test]
async fn a_cancelled_rescan_drops_the_old_report(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    cancel_held_scan(&f, cx).await;
}

#[gpui::test]
async fn stale_marks_while_hidden_cost_no_scan_and_one_on_show(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_visible(cx, true).await;
    f.set_visible(cx, false).await;
    let before = (f.status_calls(), f.stash_calls());
    for _ in 0..20 {
        f.model.update(cx, ChangesModel::mark_stale).await;
    }
    assert_eq!(f.status_calls(), before.0, "a hidden list scans nothing");
    assert_eq!(f.stash_calls(), before.1);
    assert!(f.read(cx, ChangesModel::is_stale));

    f.set_visible(cx, true).await;
    assert_eq!(f.status_calls(), before.0 + 1, "one rescan on show");
    assert_eq!(f.stash_calls(), before.1 + 1);
    assert!(
        !f.read(cx, ChangesModel::is_stale),
        "showing consumes the stale mark"
    );

    f.set_visible(cx, true).await;
    assert_eq!(
        f.status_calls(),
        before.0 + 1,
        "showing a list that is current does not rescan"
    );
}

#[gpui::test]
async fn a_visible_list_rescans_on_every_stale_mark(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_visible(cx, true).await;
    f.model.update(cx, ChangesModel::mark_stale).await;
    f.model.update(cx, ChangesModel::mark_stale).await;
    assert_eq!(f.status_calls(), 3);
    assert!(!f.read(cx, ChangesModel::is_stale));
}

#[gpui::test]
async fn a_worktree_change_rescans_the_list_but_not_the_stashes(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_visible(cx, true).await;
    let before = (f.status_calls(), f.stash_calls());
    f.model.update(cx, ChangesModel::mark_worktree_stale).await;
    assert_eq!(f.status_calls(), before.0 + 1);
    assert_eq!(
        f.stash_calls(),
        before.1,
        "the stash list lives under .git, which the ref watcher covers"
    );

    f.set_visible(cx, false).await;
    f.model.update(cx, ChangesModel::mark_worktree_stale).await;
    assert_eq!(f.status_calls(), before.0 + 1, "a hidden list waits");
    assert!(f.read(cx, ChangesModel::is_stale));
}

#[gpui::test]
async fn showing_a_never_loaded_list_loads_it(cx: &mut TestAppContext) {
    let f = fixture(cx);
    assert!(!f.read(cx, ChangesModel::is_stale));
    f.set_visible(cx, true).await;
    assert_eq!(f.status_calls(), 1, "an idle list loads on show");
}

#[gpui::test]
async fn hiding_cancels_the_scan_in_flight(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_visible(cx, true).await;
    let gate = Arc::new(CallGate::new());
    f.repo.state().status_gate = Some(Arc::clone(&gate));
    let rescan = f.model.update(cx, ChangesModel::mark_stale);
    assert!(gate.wait_until_arrived());
    f.set_visible(cx, false).await;
    assert_eq!(
        f.state(cx),
        ChangesState::Idle,
        "the list reloads when shown again"
    );
    gate.open();
    rescan.await;
    assert_eq!(
        f.state(cx),
        ChangesState::Idle,
        "the cancelled scan's result never lands"
    );
    assert!(!f.read(cx, ChangesModel::is_scanning));

    f.set_visible(cx, true).await;
    assert_eq!(f.status_calls(), 3, "the idle list reloads on show");
    assert!(matches!(f.state(cx), ChangesState::Loaded(_)));
}

#[gpui::test]
async fn files_are_ticked_by_default_and_unticks_survive_refresh(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    assert!(f.read(cx, |m| m.is_ticked("a.txt")));
    assert_eq!(
        f.read(cx, ChangesModel::ticked_paths),
        ["a.txt", "b.txt", "gone.txt"]
    );

    f.set_ticked(cx, &["b.txt"], false);
    assert_eq!(
        f.read(cx, ChangesModel::ticked_paths),
        ["a.txt", "gone.txt"]
    );
    assert_eq!(
        f.read(cx, ChangesModel::selections),
        [whole("a.txt"), whole("gone.txt")]
    );

    f.load_status(cx).await;
    assert!(
        !f.read(cx, |m| m.is_ticked("b.txt")),
        "the untick survives a refresh"
    );

    // A path that vanished from the report no longer pins state.
    f.set_report(report(
        vec![entry(b"a.txt", WorktreeStatus::Modified, Some(0x01), 3)],
        1,
    ));
    f.load_status(cx).await;
    assert_eq!(f.read(cx, ChangesModel::ticked_paths), ["a.txt"]);
}

#[gpui::test]
async fn hunk_ticks_collapse_to_whole_file_and_to_unticked(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let first = stamp(&hunk((1, 3), (1, 3), "x"));
    let second = stamp(&hunk((8, 2), (8, 3), "y"));
    let all = [first, second];
    let hunks = |stamps: Vec<HunkStamp>| CommitSelection::Hunks {
        path: "a.txt".to_owned(),
        hunks: stamps,
    };

    assert!(
        f.read(cx, |m| m.is_hunk_ticked("a.txt", first)),
        "every hunk of a ticked file is ticked"
    );
    f.toggle_hunk(cx, "a.txt", second, &all);
    assert!(f.read(cx, |m| m.is_partially_ticked("a.txt")));
    assert!(f.read(cx, |m| m.is_hunk_ticked("a.txt", first)));
    assert!(!f.read(cx, |m| m.is_hunk_ticked("a.txt", second)));
    assert_eq!(f.read(cx, ChangesModel::selections)[0], hunks(vec![first]));

    f.toggle_hunk(cx, "a.txt", second, &all);
    assert!(
        !f.read(cx, |m| m.is_partially_ticked("a.txt")),
        "all hunks ticked collapses to whole"
    );
    assert_eq!(f.read(cx, ChangesModel::selections)[0], whole("a.txt"));

    f.toggle_hunk(cx, "a.txt", first, &all);
    f.toggle_hunk(cx, "a.txt", second, &all);
    assert!(
        !f.read(cx, |m| m.is_ticked("a.txt")),
        "no hunks left unticks the file"
    );

    f.toggle_hunk(cx, "a.txt", first, &all);
    assert!(
        f.read(cx, |m| m.is_ticked("a.txt")),
        "ticking a hunk of an unticked file re-ticks it"
    );
    assert_eq!(f.read(cx, ChangesModel::selections)[0], hunks(vec![first]));
}

fn folder_report() -> StatusReport {
    let paths: [&[u8]; 4] = [b"src/a.txt", b"src/b.txt", b"src/deep/c.txt", b"top.txt"];
    report(
        paths
            .into_iter()
            .map(|path| entry(path, WorktreeStatus::Modified, Some(0x01), 3))
            .collect(),
        4,
    )
}

fn summary(text: &str, has_selection: bool) -> TickSummary {
    TickSummary {
        text: text.to_owned(),
        has_selection,
    }
}

#[gpui::test]
async fn row_ticks_count_every_file_beneath_a_folder(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_report(folder_report());
    f.load_status(cx).await;
    assert_eq!(
        f.row_tick(cx, "src/"),
        RowTick {
            state: TickState::On,
            is_enabled: true
        }
    );
    assert_eq!(
        f.read(cx, ChangesModel::tick_summary),
        summary("4 of 4 selected", true)
    );

    f.set_ticked(cx, &["src/deep/c.txt"], false);
    assert_eq!(
        f.row_tick(cx, "src/").state,
        TickState::Mixed,
        "some files ticked"
    );
    assert_eq!(f.row_tick(cx, "src/deep/").state, TickState::Off);
    assert_eq!(f.row_tick(cx, "src/a.txt").state, TickState::On);
    assert_eq!(
        f.read(cx, ChangesModel::tick_summary),
        summary("3 of 4 selected", true)
    );

    f.set_ticked(cx, &["src/a.txt", "src/b.txt", "top.txt"], false);
    assert_eq!(
        f.row_tick(cx, "src/").state,
        TickState::Off,
        "no file ticked"
    );
    assert_eq!(
        f.read(cx, ChangesModel::tick_summary),
        summary("0 of 4 selected", false)
    );
}

#[gpui::test]
async fn a_file_with_some_hunks_ticked_is_mixed_and_so_is_its_folder(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_report(folder_report());
    f.load_status(cx).await;
    let (first, second) = (stamp(&first_hunk()), stamp(&second_hunk()));
    f.toggle_hunk(cx, "src/a.txt", second, &[first, second]);
    assert_eq!(f.row_tick(cx, "src/a.txt").state, TickState::Mixed);
    assert_eq!(f.row_tick(cx, "src/").state, TickState::Mixed);
    assert_eq!(
        f.read(cx, ChangesModel::tick_summary).text,
        "4 of 4 selected",
        "a partial file is still selected"
    );

    f.toggle_hunk(cx, "src/a.txt", second, &[first, second]);
    assert_eq!(f.row_tick(cx, "src/a.txt").state, TickState::On);
    assert_eq!(f.row_tick(cx, "src/").state, TickState::On);
}

#[gpui::test]
async fn a_row_of_only_inexact_paths_takes_no_click(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.set_report(report(
        vec![
            entry(b"dir/f\xFFo", WorktreeStatus::Deleted, Some(0x02), 0),
            entry(b"top.txt", WorktreeStatus::Untracked, None, 1),
        ],
        2,
    ));
    f.load_status(cx).await;
    let off = RowTick {
        state: TickState::Off,
        is_enabled: false,
    };
    assert_eq!(f.row_tick(cx, "dir/"), off);
    assert_eq!(f.row_tick(cx, "dir/f\u{FFFD}o"), off);
    assert_eq!(
        f.read(cx, ChangesModel::tick_summary),
        summary("1 of 2 selected", true)
    );
}

#[gpui::test]
async fn an_empty_list_has_no_summary_and_nothing_to_commit(cx: &mut TestAppContext) {
    let f = fixture(cx);
    assert_eq!(
        f.read(cx, ChangesModel::tick_summary),
        summary("", false),
        "not loaded"
    );
    f.set_report(report(Vec::new(), 0));
    f.load_status(cx).await;
    assert_eq!(f.read(cx, ChangesModel::tick_summary), summary("", false));
}

#[test]
fn a_hunk_stamp_carries_each_header_range_in_its_own_field() {
    let header = hunk((3, 5), (7, 11), "x");
    assert_eq!(
        stamp(&header),
        HunkStamp {
            old_start: 3,
            old_lines: 5,
            new_start: 7,
            new_lines: 11
        }
    );
}

fn report_with_a_txt(a_txt: bool) -> StatusReport {
    let mut entries = vec![entry(b"b.txt", WorktreeStatus::Untracked, None, 2)];
    if a_txt {
        entries.insert(0, entry(b"a.txt", WorktreeStatus::Modified, Some(0x01), 3));
    }
    report(entries, 2)
}

fn a_txt_hunks(stamps: Vec<HunkStamp>) -> CommitSelection {
    CommitSelection::Hunks {
        path: "a.txt".to_owned(),
        hunks: stamps,
    }
}

fn set_diff(repo: &FakeRepository, path: &str, diff: BlobDiff) {
    repo.state().worktree_diffs.insert(path.to_owned(), diff);
}

#[gpui::test]
async fn a_hunk_tick_that_no_longer_matches_is_dropped_on_refresh(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (first, second) = (stamp(&first_hunk()), stamp(&second_hunk()));
    f.load_status(cx).await;
    f.toggle_hunk(cx, "a.txt", second, &[first, second]);
    assert_eq!(
        f.read(cx, ChangesModel::selections)[0],
        a_txt_hunks(vec![first])
    );

    // The ticked hunk was committed; only the unticked one is left on disk.
    set_diff(&f.repo, "a.txt", text(vec![second_hunk()]));
    f.load_status(cx).await;
    assert!(
        !f.read(cx, |m| m.is_ticked("a.txt")),
        "never widened to hunks the user did not tick"
    );
    assert_eq!(
        f.read(cx, ChangesModel::selections),
        [whole("b.txt"), whole("gone.txt")]
    );
}

#[gpui::test]
async fn a_hunk_tick_whose_diff_cannot_be_read_is_dropped_and_reported(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (first, second) = (stamp(&first_hunk()), stamp(&second_hunk()));
    let events = f.events(cx);
    f.load_status(cx).await;
    f.toggle_hunk(cx, "a.txt", second, &[first, second]);
    f.load_status(cx).await;
    assert!(!f.read(cx, |m| m.is_ticked("a.txt")));
    let errors: Vec<ChangesEvent> = events
        .borrow()
        .iter()
        .filter(|event| matches!(event, ChangesEvent::Error(_)))
        .cloned()
        .collect();
    assert_eq!(
        errors,
        [ChangesEvent::Error(
            "read working tree file: 'a.txt': missing".to_owned()
        )]
    );
}

#[gpui::test]
async fn partial_hunk_ticks_survive_a_refresh_and_drop_with_their_path(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (first, second) = (stamp(&first_hunk()), stamp(&second_hunk()));
    f.set_report(report_with_a_txt(true));
    set_diff(&f.repo, "a.txt", text(vec![first_hunk(), second_hunk()]));
    f.load_status(cx).await;
    f.toggle_hunk(cx, "a.txt", second, &[first, second]);

    f.load_status(cx).await;
    assert!(f.read(cx, |m| m.is_partially_ticked("a.txt")));
    assert_eq!(
        f.read(cx, ChangesModel::selections)[0],
        a_txt_hunks(vec![first])
    );

    f.set_report(report_with_a_txt(false));
    f.load_status(cx).await;
    f.set_report(report_with_a_txt(true));
    f.load_status(cx).await;
    assert!(!f.read(cx, |m| m.is_partially_ticked("a.txt")));
    assert_eq!(f.read(cx, ChangesModel::selections)[0], whole("a.txt"));
}

#[gpui::test]
async fn a_later_file_selection_wins(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let stale = text(vec![hunk((1, 1), (1, 1), "stale")]);
    let later = text(vec![hunk((0, 0), (1, 1), "later")]);
    set_diff(&f.repo, "a.txt", stale.clone());
    set_diff(&f.repo, "b.txt", later.clone());
    let entries = f.entries(cx);
    let shown = f.shown_diffs(cx);
    let gate = Arc::new(CallGate::new());
    f.repo
        .state()
        .worktree_diff_gates
        .insert("a.txt".to_owned(), Arc::clone(&gate));
    let first = entries[0].clone();
    let slow = f.model.update(cx, |m, cx| m.select(first, cx));
    assert!(gate.wait_until_arrived());
    f.select(cx, entries[1].clone()).await;
    gate.open();
    slow.await;

    assert_eq!(
        f.read(cx, |m| m.selected_path().map(str::to_owned)),
        Some("b.txt".to_owned())
    );
    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Loaded("b.txt".to_owned(), later)
    );
    assert!(
        !shown
            .borrow()
            .contains(&WorktreeDiffState::Loaded("a.txt".to_owned(), stale)),
        "the slow selection's diff never rendered"
    );
}

#[gpui::test]
async fn selecting_a_file_loads_its_worktree_diff(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let diff = text(vec![hunk((1, 1), (1, 1), "x")]);
    set_diff(&f.repo, "a.txt", diff.clone());
    f.repo
        .state()
        .blob_diffs
        .insert((Some(oid(0x02)), None), text(Vec::new()));
    let entries = f.entries(cx);

    f.select(cx, entries[0].clone()).await;
    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Loaded("a.txt".to_owned(), diff)
    );

    // A deleted file diffs through the blob pair, old side only.
    f.select(cx, entries[2].clone()).await;
    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Loaded("gone.txt".to_owned(), text(Vec::new()))
    );

    // A vanished selection resets the pane on refresh.
    f.set_report(report(Vec::new(), 0));
    f.load_status(cx).await;
    assert_eq!(f.read(cx, |m| m.selected_path().map(str::to_owned)), None);
    assert_eq!(f.diff(cx), WorktreeDiffState::Idle);
}

#[gpui::test]
async fn a_rescan_reloads_the_selected_diff_without_showing_loading(cx: &mut TestAppContext) {
    let f = fixture(cx);
    set_diff(&f.repo, "a.txt", text(vec![first_hunk()]));
    f.load_status(cx).await;
    let first = f.entries(cx)[0].clone();
    f.select(cx, first).await;
    let shown = f.shown_diffs(cx);

    let fresh = text(vec![second_hunk()]);
    set_diff(&f.repo, "a.txt", fresh.clone());
    f.load_status(cx).await;
    let loaded = WorktreeDiffState::Loaded("a.txt".to_owned(), fresh);
    assert_eq!(f.diff(cx), loaded);
    assert_eq!(*shown.borrow(), [loaded], "never back through loading");

    f.load_status(cx).await;
    assert_eq!(
        shown.borrow().len(),
        1,
        "an unchanged diff does not re-render"
    );
}

#[gpui::test]
async fn a_rescan_reads_a_selected_partially_ticked_file_once(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let (first, second) = (stamp(&first_hunk()), stamp(&second_hunk()));
    let diff = text(vec![first_hunk(), second_hunk()]);
    f.set_report(report_with_a_txt(true));
    set_diff(&f.repo, "a.txt", diff.clone());
    f.load_status(cx).await;
    let a_txt = f.entries(cx)[0].clone();
    f.select(cx, a_txt).await;
    f.toggle_hunk(cx, "a.txt", second, &[first, second]);
    let reads = |f: &Fixture| f.repo.state().worktree_diff_calls["a.txt"];
    let before = reads(&f);

    f.load_status(cx).await;
    assert_eq!(
        reads(&f) - before,
        1,
        "the tick check and the pane share one read"
    );
    assert!(f.read(cx, |m| m.is_partially_ticked("a.txt")));
    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Loaded("a.txt".to_owned(), diff)
    );
}

#[gpui::test]
async fn a_diff_that_cannot_be_read_shows_the_error_for_that_path(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let events = f.events(cx);
    let second = f.entries(cx)[1].clone();
    f.select(cx, second).await;
    let WorktreeDiffState::Failed(path, message) = f.diff(cx) else {
        panic!("expected a failed diff");
    };
    assert_eq!(path, "b.txt");
    assert!(message.contains("read working tree file"));
    assert_eq!(
        count(&events, &ChangesEvent::DiffChanged),
        2,
        "one for loading, one for failed"
    );
}

#[gpui::test]
async fn stashes_load(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    let events = f.events(cx);
    f.load_stashes(cx).await;
    assert_eq!(f.read(cx, |m| m.stashes().len()), 1);
    assert_eq!(
        f.read(cx, |m| m.stash_state().clone()),
        StashListState::Loaded
    );
    assert_eq!(count(&events, &ChangesEvent::StashChanged), 1);
}

#[gpui::test]
async fn the_stash_selection_follows_its_commit_when_a_push_shifts_the_rows(
    cx: &mut TestAppContext,
) {
    let f = fixture(cx);
    f.repo.state().stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    f.load_stashes(cx).await;
    f.model.update(cx, |m, _| m.select_stash(Some(0)));
    assert_eq!(f.read(cx, ChangesModel::selected_stash_index), Some(0));

    f.repo.state().stashes = vec![
        stash(0x0D, 0, "WIP on main: y"),
        stash(0x0C, 1, "WIP on main: x"),
    ];
    f.load_stashes(cx).await;
    assert_eq!(
        f.read(cx, ChangesModel::selected_stash_index),
        Some(1),
        "the pick stays on the commit, not the row"
    );
}

#[gpui::test]
async fn the_stash_selection_clears_when_its_stash_is_gone(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    f.load_stashes(cx).await;
    f.model.update(cx, |m, _| m.select_stash(Some(0)));
    f.repo.state().stashes = vec![stash(0x0D, 0, "WIP on main: y")];
    f.load_stashes(cx).await;
    assert_eq!(f.read(cx, ChangesModel::selected_stash_index), None);

    f.model.update(cx, |m, _| m.select_stash(Some(5)));
    assert_eq!(
        f.read(cx, ChangesModel::selected_stash_index),
        None,
        "a row past the list picks nothing"
    );
    f.model.update(cx, |m, _| {
        m.select_stash(Some(0));
        m.select_stash(None);
    });
    assert_eq!(f.read(cx, ChangesModel::selected_stash_index), None);
}

#[gpui::test]
async fn a_failed_stash_read_shows_no_selection(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    f.load_stashes(cx).await;
    f.model.update(cx, |m, _| m.select_stash(Some(0)));
    f.repo.state().fail_next_stash = true;
    f.load_stashes(cx).await;
    assert_eq!(
        f.read(cx, ChangesModel::selected_stash_index),
        None,
        "the view shows the error, not the list"
    );
}

#[gpui::test]
async fn a_failed_stash_list_surfaces_as_an_error_and_keeps_the_last_list(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    f.load_stashes(cx).await;
    let events = f.events(cx);
    let states = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&states);
    cx.update(|cx| {
        cx.subscribe(&f.model, move |model, event: &ChangesEvent, cx| {
            if *event == ChangesEvent::StashChanged {
                sink.borrow_mut().push(model.read(cx).stash_state().clone());
            }
        })
        .detach();
    });
    f.repo.state().fail_next_stash = true;
    f.load_stashes(cx).await;

    let errors: Vec<_> = events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            ChangesEvent::Error(message) => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("list stashes"));
    assert_eq!(
        f.read(cx, |m| m.stashes().len()),
        1,
        "the last list read stays for the view to set aside"
    );
    let StashListState::Failed(message) = f.read(cx, |m| m.stash_state().clone()) else {
        panic!("expected failed");
    };
    assert!(message.contains("list stashes"));
    assert_eq!(
        *states.borrow(),
        [StashListState::Failed(message)],
        "the failure is announced for inline display"
    );

    f.load_stashes(cx).await;
    assert_eq!(
        f.read(cx, |m| m.stash_state().clone()),
        StashListState::Loaded,
        "a successful read clears the failure"
    );
    assert_eq!(states.borrow().last(), Some(&StashListState::Loaded));
}

#[gpui::test]
async fn a_stash_failure_that_persists_across_refreshes_alerts_once(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let events = f.events(cx);
    f.repo.state().fail_next_stash = true;
    f.refresh(cx).await;
    f.repo.state().fail_next_stash = true;
    f.refresh(cx).await;

    assert_eq!(f.stash_calls(), 2);
    let errors = events
        .borrow()
        .iter()
        .filter(|event| matches!(event, ChangesEvent::Error(_)))
        .count();
    assert_eq!(
        errors, 1,
        "one alert per distinct message, not one per refresh"
    );
    assert!(matches!(
        f.read(cx, |m| m.stash_state().clone()),
        StashListState::Failed(_)
    ));
}

#[gpui::test]
async fn a_burst_of_stash_requests_costs_one_reload(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let gate = Arc::new(CallGate::new());
    f.repo.state().stash_gate = Some(Arc::clone(&gate));
    let in_flight = f.model.update(cx, ChangesModel::load_stashes);
    assert!(gate.wait_until_arrived());
    for _ in 0..20 {
        f.load_stashes(cx).await;
    }
    gate.open();
    in_flight.await;
    assert_eq!(
        f.stash_calls(),
        2,
        "the read in flight plus one re-run, however many requests arrived"
    );
}

#[gpui::test]
async fn a_cancelled_stash_load_never_lands(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.repo.state().stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    let gate = Arc::new(CallGate::new());
    f.repo.state().stash_gate = Some(Arc::clone(&gate));
    let events = f.events(cx);
    let in_flight = f.model.update(cx, ChangesModel::load_stashes);
    assert!(gate.wait_until_arrived());
    f.load_stashes(cx).await;
    f.model.update(cx, ChangesModel::cancel_scans);
    gate.open();
    in_flight.await;
    assert!(
        f.read(cx, |m| m.stashes().is_empty()),
        "the cancelled read's result never lands"
    );
    assert_eq!(count(&events, &ChangesEvent::StashChanged), 0);
    assert_eq!(f.stash_calls(), 1, "a cancel drops the queued re-run");

    f.load_stashes(cx).await;
    assert_eq!(
        f.read(cx, |m| m.stashes().len()),
        1,
        "the next read is clean"
    );
    assert_eq!(f.stash_calls(), 2);
}

#[gpui::test]
async fn a_refresh_burst_costs_two_of_each(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let status_gate = Arc::new(CallGate::new());
    let stash_gate = Arc::new(CallGate::new());
    {
        let mut state = f.repo.state();
        state.status_gate = Some(Arc::clone(&status_gate));
        state.stash_gate = Some(Arc::clone(&stash_gate));
    }
    let in_flight = f.model.update(cx, ChangesModel::refresh);
    assert!(status_gate.wait_until_arrived());
    assert!(stash_gate.wait_until_arrived());
    for _ in 0..20 {
        f.refresh(cx).await;
    }
    status_gate.open();
    stash_gate.open();
    in_flight.await;
    assert_eq!(
        f.status_calls(),
        2,
        "the scan in flight plus one re-run, however many refreshes arrived"
    );
    assert_eq!(
        f.stash_calls(),
        2,
        "the stash read in flight plus one re-run, however many refreshes arrived"
    );
}

#[gpui::test]
async fn refresh_loads_the_change_list_and_the_stashes(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let stashes = vec![stash(0x0C, 0, "WIP on main: x")];
    f.repo.state().stashes = stashes.clone();
    f.refresh(cx).await;
    assert_eq!(f.state(cx), ChangesState::Loaded(standard_report()));
    assert_eq!(f.read(cx, |m| m.stashes().to_vec()), stashes);
}

#[gpui::test]
async fn a_reselected_files_older_diff_never_lands(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    let a_txt = f.entries(cx)[0].clone();
    set_diff(&f.repo, "a.txt", text(vec![first_hunk()]));
    let gate = Arc::new(CallGate::new());
    f.repo
        .state()
        .worktree_diff_gates
        .insert("a.txt".to_owned(), Arc::clone(&gate));
    let shown = f.shown_diffs(cx);
    let held = a_txt.clone();
    let slow = f.model.update(cx, |m, cx| m.select(held, cx));
    assert!(gate.wait_until_arrived());
    // Same path, newer content: only the generation tells the two reads apart.
    set_diff(&f.repo, "a.txt", text(vec![second_hunk()]));
    f.select(cx, a_txt).await;
    gate.open();
    slow.await;

    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Loaded("a.txt".to_owned(), text(vec![second_hunk()]))
    );
    assert!(
        !shown.borrow().contains(&WorktreeDiffState::Loaded(
            "a.txt".to_owned(),
            text(vec![first_hunk()])
        )),
        "the older read never rendered"
    );
}

#[gpui::test]
async fn a_diff_for_a_file_that_left_the_list_never_lands(cx: &mut TestAppContext) {
    let f = fixture(cx);
    f.load_status(cx).await;
    set_diff(&f.repo, "a.txt", text(vec![first_hunk()]));
    let gate = Arc::new(CallGate::new());
    f.repo
        .state()
        .worktree_diff_gates
        .insert("a.txt".to_owned(), Arc::clone(&gate));
    let a_txt = f.entries(cx)[0].clone();
    let slow = f.model.update(cx, |m, cx| m.select(a_txt, cx));
    assert!(gate.wait_until_arrived());
    f.set_report(report_with_a_txt(false));
    f.load_status(cx).await;
    gate.open();
    slow.await;

    assert_eq!(f.read(cx, |m| m.selected_path().map(str::to_owned)), None);
    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Idle,
        "the vanished file's diff never lands"
    );
}

#[gpui::test]
async fn the_grouping_is_remembered_and_shapes_the_rows(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let shared = settings();
    let repo = FakeRepository::new();
    repo.state().status_report = folder_report();
    let reading = Arc::new(repo.clone());
    let model = cx.new(|_| {
        ChangesModel::new(
            reading,
            ChangeGroupingStore::worktree(Arc::clone(&shared)),
            false,
        )
    });
    assert_eq!(
        model.read_with(cx, |m, _| m.grouping()),
        ChangeGrouping::Flat
    );
    model.update(cx, ChangesModel::load_status).await;
    assert_eq!(
        model.read_with(cx, |m, _| m.nodes().len()),
        4,
        "one row per file"
    );
    model
        .update(cx, |m, cx| m.set_grouping(ChangeGrouping::Folders, cx))
        .unwrap();
    assert_eq!(
        model.read_with(cx, |m, _| m.nodes().len()),
        2,
        "src/ and top.txt"
    );
    let reopened = ChangeGroupingStore::worktree(shared);
    assert_eq!(reopened.grouping(), ChangeGrouping::Folders);
}

#[gpui::test]
async fn a_rescans_older_diff_read_never_lands_over_a_newer_selection(cx: &mut TestAppContext) {
    let f = fixture(cx);
    set_diff(&f.repo, "a.txt", text(vec![first_hunk()]));
    f.load_status(cx).await;
    let a_txt = f.entries(cx)[0].clone();
    f.select(cx, a_txt.clone()).await;
    let shown = f.shown_diffs(cx);

    let older = text(vec![second_hunk()]);
    set_diff(&f.repo, "a.txt", older.clone());
    let gate = Arc::new(CallGate::new());
    f.repo
        .state()
        .worktree_diff_gates
        .insert("a.txt".to_owned(), Arc::clone(&gate));
    let rescan = f.model.update(cx, ChangesModel::load_status);
    // The rescan reads the selected diff once its scan lands; drive it there.
    let deadline = Instant::now() + Duration::from_secs(20);
    while f.repo.state().worktree_diff_calls["a.txt"] < 2 {
        assert!(Instant::now() < deadline, "the reload never started");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(gate.wait_until_arrived());
    // Same path, newer content: only the generation tells the two reads apart.
    let newer = text(vec![first_hunk(), second_hunk()]);
    set_diff(&f.repo, "a.txt", newer.clone());
    f.select(cx, a_txt).await;
    gate.open();
    rescan.await;

    assert_eq!(
        f.diff(cx),
        WorktreeDiffState::Loaded("a.txt".to_owned(), newer)
    );
    assert!(
        !shown
            .borrow()
            .contains(&WorktreeDiffState::Loaded("a.txt".to_owned(), older)),
        "the rescan's older read never rendered"
    );
}
