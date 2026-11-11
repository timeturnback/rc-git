//! The worktree watcher's half of `ChangesModel`, which the Swift window controller wired:
//! events reach the model through `ManualWatchers`, and however many arrive during one slow
//! scan, they cost exactly one more.
#![expect(clippy::unwrap_used, reason = "the manual watcher always starts")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext, Entity, TestAppContext};
use rc_git_core::bstr::BString;
use rc_git_core::{EntryKind, StatusEntry, StatusReport, WorktreeStatus};
use rc_git_kit::changes::ChangesModel;
use rc_git_kit::fake::{CallGate, FakeRepository};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::ChangeGroupingStore;
use rc_git_kit::platform::watch::{ManualWatchers, WorktreeDirs};

const WORK_DIR: &str = "/nonexistent-rc-git-checkout";

fn dirs() -> WorktreeDirs {
    WorktreeDirs {
        work_dir: WORK_DIR.to_owned(),
        git_dir: format!("{WORK_DIR}/.git"),
        common_dir: format!("{WORK_DIR}/.git"),
    }
}

fn under(path: &str) -> String {
    format!("{WORK_DIR}/{path}")
}

struct Watched {
    repo: FakeRepository,
    watchers: ManualWatchers,
    model: Entity<ChangesModel>,
}

async fn watched(cx: &mut TestAppContext) -> Watched {
    cx.executor().allow_parking();
    let repo = FakeRepository::new();
    repo.state().status_report = StatusReport {
        entries: vec![StatusEntry {
            path: BString::from("a.txt"),
            kind: EntryKind::Blob,
            status: WorktreeStatus::Untracked,
            oid: None,
            size: 1,
        }],
        scanned: 1,
        ignored_dirs: vec![BString::from("build")],
        ..StatusReport::default()
    };
    let reading = Arc::new(repo.clone());
    let grouping = ChangeGroupingStore::worktree(Arc::new(MemorySettings::new()));
    let model = cx.new(|_| ChangesModel::new(reading, grouping, false));
    let watchers = ManualWatchers::new();
    let factory = watchers.clone();
    model
        .update(cx, |m, cx| m.watch_worktree(&factory, &dirs(), cx))
        .unwrap();
    model.update(cx, |m, cx| m.set_visible(true, cx)).await;
    Watched {
        repo,
        watchers,
        model,
    }
}

impl Watched {
    fn status_calls(&self) -> usize {
        self.repo.state().status_calls
    }

    fn emit(&self, cx: &mut TestAppContext, path: &str) -> usize {
        let fired = self.watchers.emit(&[&under(path)]);
        cx.run_until_parked();
        fired
    }

    /// Drives the executor until no scan runs, failing after a generous bound.
    fn settle(&self, cx: &mut TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            cx.run_until_parked();
            if !self.model.read_with(cx, |m, _| m.is_scanning()) {
                return;
            }
            assert!(Instant::now() < deadline, "the scan loop never ended");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[gpui::test]
async fn a_burst_of_watcher_events_during_one_slow_scan_costs_two_scans(cx: &mut TestAppContext) {
    let w = watched(cx).await;
    assert_eq!(w.status_calls(), 1, "showing the list loaded it");
    let gate = Arc::new(CallGate::new());
    w.repo.state().status_gate = Some(Arc::clone(&gate));
    assert_eq!(w.emit(cx, "a.txt"), 1);
    assert!(gate.wait_until_arrived(), "the event started a scan");
    for _ in 0..50 {
        w.emit(cx, "a.txt");
    }
    gate.open();
    w.settle(cx);
    assert_eq!(
        w.status_calls(),
        3,
        "the load on show, the scan in flight, and one re-run for the whole burst"
    );
    assert_eq!(
        w.repo.state().stash_calls,
        1,
        "a worktree event never re-reads the stashes"
    );
}

#[gpui::test]
async fn events_while_hidden_cost_one_scan_on_show(cx: &mut TestAppContext) {
    let w = watched(cx).await;
    w.model.update(cx, |m, cx| m.set_visible(false, cx)).await;
    for _ in 0..10 {
        w.emit(cx, "a.txt");
    }
    w.settle(cx);
    assert_eq!(w.status_calls(), 1, "a hidden list scans nothing");
    assert!(w.model.read_with(cx, |m, _| m.is_stale()));
    w.model.update(cx, |m, cx| m.set_visible(true, cx)).await;
    assert_eq!(w.status_calls(), 2);
}

#[gpui::test]
async fn a_scan_hands_its_pruned_directories_to_the_watcher(cx: &mut TestAppContext) {
    let w = watched(cx).await;
    assert_eq!(
        w.emit(cx, "build/out.o"),
        0,
        "an event below a pruned ignored directory is dropped"
    );
    assert_eq!(
        w.emit(cx, "build"),
        1,
        "the directory itself stays relevant"
    );
    assert_eq!(
        w.emit(cx, ".git/objects/ab"),
        0,
        "git's own churn is dropped"
    );
    w.settle(cx);
}

#[gpui::test]
async fn stopping_the_watcher_stops_the_rescans(cx: &mut TestAppContext) {
    let w = watched(cx).await;
    w.model.update(cx, |m, _| m.stop_watching());
    assert_eq!(w.emit(cx, "a.txt"), 0);
    w.settle(cx);
    assert_eq!(w.status_calls(), 1);
}
