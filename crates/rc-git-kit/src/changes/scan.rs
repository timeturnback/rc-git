//! The change model's loads: the status scan and the stash read, each coalesced to one
//! running plus one pending, and the diff pane's read, guarded by its generation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, Task};
use rc_git_core::bstr::ByteSlice;
use rc_git_core::{
    BlobDiff, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT, Oid, StashEntry, StatusEntry, StatusReport,
    WorktreeStatus,
};

use super::{ChangesEvent, ChangesModel, ChangesState, StashListState, WorktreeDiffState, prune};
use crate::blocking::blocking;
use crate::hidden_files;
use crate::models::path_refusal;
use crate::repository::{DEFAULT_STATUS_SCAN_CAP, ReadResult, RepositoryError, RepositoryReading};

/// Per partially ticked path, the diff a scan read for it.
type Diffs = HashMap<String, ReadResult<BlobDiff>>;

/// What one status scan read: the listed report, the `HEAD` it was built against, and the
/// diffs of the partially ticked files.
struct Scanned {
    report: StatusReport,
    head: Option<Oid>,
    diffs: Diffs,
}

/// One scan handed to its thread, and the generation it carries.
struct Pending<T> {
    generation: u64,
    work: Task<ReadResult<T>>,
}

/// The report as the list shows it: without untracked hidden files unless `shows_hidden`. A
/// tracked hidden file that changed stays, since hiding it would hide a real edit.
pub(super) fn listed(mut report: StatusReport, shows_hidden: bool) -> StatusReport {
    if !shows_hidden {
        report.entries.retain(|entry| {
            entry.status != WorktreeStatus::Untracked
                || !hidden_files::is_hidden_path(entry.path.as_bytes())
        });
    }
    report
}

/// `entry`'s diff against `HEAD`: the old blob alone for a deleted file, else the file on
/// disk, which is refused when its path is not exact.
fn worktree_diff(entry: &StatusEntry, repository: &dyn RepositoryReading) -> ReadResult<BlobDiff> {
    if entry.status == WorktreeStatus::Deleted {
        return repository.diff_blobs(entry.oid, None, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT);
    }
    let Ok(path) = entry.path.to_str() else {
        let lossy = entry.path.to_str_lossy();
        return Err(RepositoryError::new(path_refusal("diff", &lossy)));
    };
    repository.diff_worktree_file(path, entry.oid, DEFAULT_BLOB_LIMIT, DEFAULT_DIFF_CONTEXT)
}

/// Each partially ticked path's current diff, read from the fresh report's entry: one diff
/// per partially ticked file, so the cost is bounded by what the user picked, never by the
/// repository. The selected file's read is handed to the pane rather than read twice.
fn partial_diffs(
    paths: &HashSet<String>,
    report: &StatusReport,
    repository: &dyn RepositoryReading,
) -> Diffs {
    let mut diffs = HashMap::new();
    for entry in &report.entries {
        let path = entry.path.to_str_lossy();
        if paths.contains(path.as_ref()) {
            diffs.insert(path.into_owned(), worktree_diff(entry, repository));
        }
    }
    diffs
}

/// One whole scan: one `HEAD` read feeds both the tree and the expected head, so they always
/// agree.
fn scan(
    repository: &dyn RepositoryReading,
    shows_hidden: bool,
    partial: &HashSet<String>,
) -> ReadResult<Scanned> {
    let head = repository.head()?.target;
    let tree = head.map(|target| repository.tree_of(target)).transpose()?;
    let report = listed(
        repository.worktree_status(tree, DEFAULT_STATUS_SCAN_CAP)?,
        shows_hidden,
    );
    let diffs = partial_diffs(partial, &report, repository);
    Ok(Scanned {
        report,
        head,
        diffs,
    })
}

/// What the pane shows for a diff read of `path`.
fn pane_state(path: String, read: ReadResult<BlobDiff>) -> WorktreeDiffState {
    match read {
        Ok(diff) => WorktreeDiffState::Loaded(path, diff),
        Err(error) => WorktreeDiffState::Failed(path, error.message().to_owned()),
    }
}

impl ChangesModel {
    /// Reloads only the change list, capturing the `HEAD` it was built against.
    ///
    /// Scans are coalesced. A call made while one is running does not start a second: it
    /// marks the running scan's result stale and returns, and the running scan re-reads the
    /// working tree once when it lands. A watcher firing faster than the tree can be walked
    /// therefore costs one scan in flight plus one queued, never one per event. Because the
    /// request is merged rather than queued, the task a coalesced call returns is already
    /// done: it means the model knows to do it.
    pub fn load_status(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.status.pending = true;
        if self.status.running {
            return Task::ready(());
        }
        self.status.running = true;
        let first = self.begin_status_scan(cx);
        cx.spawn(async move |this, cx| {
            let mut next = first;
            while let Some(scan) = next {
                let scanned = scan.work.await;
                let landed = this.update(cx, |model, cx| {
                    model.land_status(scan.generation, scanned, cx)
                });
                let Ok(reload) = landed else { return };
                if let Some(reload) = reload {
                    reload.await;
                }
                let Ok(following) = this.update(cx, Self::begin_status_scan) else {
                    return;
                };
                next = following;
            }
        })
    }

    /// Starts the next scan when one is pending, else ends the loop. A rescan keeps the last
    /// loaded report until the new one lands, so the list and the commit and stash
    /// selections stay what the user sees; acting on it is safe, because the guard
    /// re-checks the expected head and every hunk stamp.
    fn begin_status_scan(&mut self, cx: &mut Context<Self>) -> Option<Pending<Scanned>> {
        if !self.status.pending {
            self.status.running = false;
            return None;
        }
        self.status.pending = false;
        self.status.generation += 1;
        if self.report().is_none() {
            self.set_status_state(ChangesState::Loading);
            cx.emit(ChangesEvent::StatusChanged);
            cx.notify();
        }
        let repository = Arc::clone(&self.repository);
        let shows_hidden = self.shows_hidden_files;
        let partial = self.ticks.partial_paths();
        let work = blocking(cx.background_executor(), move || {
            scan(repository.as_ref(), shows_hidden, &partial)
        });
        Some(Pending {
            generation: self.status.generation,
            work,
        })
    }

    /// Applies a scan unless a newer one or a cancel superseded it, and returns the reload of
    /// the selected file's diff when one is still needed.
    fn land_status(
        &mut self,
        generation: u64,
        scanned: ReadResult<Scanned>,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        if generation != self.status.generation {
            return None;
        }
        let reload = match scanned {
            Ok(scanned) => self.apply_scan(scanned, cx),
            Err(error) => {
                self.set_status_state(ChangesState::Failed(error.message().to_owned()));
                None
            }
        };
        cx.emit(ChangesEvent::StatusChanged);
        cx.notify();
        let (entry, read) = reload?;
        self.reload_diff(entry, read, cx)
    }

    /// Takes in a successful scan; returns the selected entry with the diff the scan read for
    /// it, when the selection is still listed.
    fn apply_scan(
        &mut self,
        mut scanned: Scanned,
        cx: &mut Context<Self>,
    ) -> Option<(StatusEntry, Option<ReadResult<BlobDiff>>)> {
        self.ticks.apply_report(&scanned.report);
        for message in self.ticks.revalidate(&scanned.diffs) {
            cx.emit(ChangesEvent::Error(message));
        }
        self.expected_head = scanned.head;
        if let Some(watching) = &self.watching {
            prune(watching.watcher.as_ref(), &scanned.report);
        }
        let selected = self.selected_path.clone().and_then(|path| {
            let entry = scanned
                .report
                .entries
                .iter()
                .find(|entry| entry.path.to_str_lossy() == path.as_str())
                .cloned();
            entry.map(|entry| (entry, scanned.diffs.remove(&path)))
        });
        if selected.is_none() && self.selected_path.take().is_some() {
            self.diff_state = WorktreeDiffState::Idle;
            cx.emit(ChangesEvent::DiffChanged);
        }
        self.set_status_state(ChangesState::Loaded(scanned.report));
        selected
    }

    /// Re-reads the selected file's diff from a fresh scan's entry, whose blob follows a
    /// moved `HEAD`, or takes `read` when the scan already read that entry for its hunk
    /// ticks. Unlike [`Self::select`] it never shows loading, so a watcher-driven rescan does
    /// not flash the placeholder, and it reports a change only when what the pane shows
    /// changed, so an unchanged file keeps its table and scroll.
    fn reload_diff(
        &mut self,
        entry: StatusEntry,
        read: Option<ReadResult<BlobDiff>>,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        self.diff_generation += 1;
        let generation = self.diff_generation;
        let path = entry.path.to_str_lossy().into_owned();
        if let Some(read) = read {
            self.land_reloaded_diff(generation, pane_state(path, read), cx);
            return None;
        }
        let pending = self.read_diff_state(entry, cx);
        Some(cx.spawn(async move |this, cx| {
            let fresh = pending.await;
            let _ = this.update(cx, |model, cx| {
                model.land_reloaded_diff(generation, fresh, cx);
            });
        }))
    }

    fn land_reloaded_diff(
        &mut self,
        generation: u64,
        fresh: WorktreeDiffState,
        cx: &mut Context<Self>,
    ) {
        let path = match &fresh {
            WorktreeDiffState::Loaded(path, _) | WorktreeDiffState::Failed(path, _) => path,
            WorktreeDiffState::Idle | WorktreeDiffState::Loading(_) => return,
        };
        if generation != self.diff_generation
            || self.selected_path.as_deref() != Some(path.as_str())
            || fresh == self.diff_state
        {
            return;
        }
        self.diff_state = fresh;
        cx.emit(ChangesEvent::DiffChanged);
        cx.notify();
    }

    /// Shows `entry`'s diff against `HEAD` in the detail pane. A file on disk whose path is
    /// not exact is not read, since the read goes by path.
    pub fn select(&mut self, entry: StatusEntry, cx: &mut Context<Self>) -> Task<()> {
        let path = entry.path.to_str_lossy().into_owned();
        self.selected_path = Some(path.clone());
        self.diff_generation += 1;
        let generation = self.diff_generation;
        self.diff_state = WorktreeDiffState::Loading(path.clone());
        cx.emit(ChangesEvent::DiffChanged);
        cx.notify();
        let pending = self.read_diff_state(entry, cx);
        cx.spawn(async move |this, cx| {
            let fresh = pending.await;
            let _ = this.update(cx, |model, cx| {
                if generation != model.diff_generation
                    || model.selected_path.as_deref() != Some(path.as_str())
                {
                    return;
                }
                model.diff_state = fresh;
                cx.emit(ChangesEvent::DiffChanged);
                cx.notify();
            });
        })
    }

    /// `entry`'s diff read off the UI thread, as the state the pane shows for it.
    fn read_diff_state(
        &self,
        entry: StatusEntry,
        cx: &mut Context<Self>,
    ) -> Task<WorktreeDiffState> {
        let repository = Arc::clone(&self.repository);
        blocking(cx.background_executor(), move || {
            let read = worktree_diff(&entry, repository.as_ref());
            pane_state(entry.path.to_str_lossy().into_owned(), read)
        })
    }

    /// Reloads the stash list. Coalesced the same way as [`Self::load_status`], and with the
    /// same caveat: a coalesced call's task means the reload is accounted for, not done.
    pub fn load_stashes(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.stash.pending = true;
        if self.stash.running {
            return Task::ready(());
        }
        self.stash.running = true;
        let first = self.begin_stash_read(cx);
        cx.spawn(async move |this, cx| {
            let mut next = first;
            while let Some(read) = next {
                let entries = read.work.await;
                let Ok(following) = this.update(cx, |model, cx| {
                    model.land_stashes(read.generation, entries, cx);
                    model.begin_stash_read(cx)
                }) else {
                    return;
                };
                next = following;
            }
        })
    }

    fn begin_stash_read(&mut self, cx: &mut Context<Self>) -> Option<Pending<Vec<StashEntry>>> {
        if !self.stash.pending {
            self.stash.running = false;
            return None;
        }
        self.stash.pending = false;
        self.stash.generation += 1;
        let repository = Arc::clone(&self.repository);
        let work = blocking(cx.background_executor(), move || repository.stash_list());
        Some(Pending {
            generation: self.stash.generation,
            work,
        })
    }

    /// A failure is shown through the stash state every time and reported once per distinct
    /// message, until a read succeeds again.
    fn land_stashes(
        &mut self,
        generation: u64,
        entries: ReadResult<Vec<StashEntry>>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.stash.generation {
            return;
        }
        match entries {
            Ok(entries) => {
                if let Some(picked) = self.selected_stash
                    && !entries.iter().any(|entry| entry.oid == picked)
                {
                    self.selected_stash = None;
                }
                self.stashes = entries;
                self.stash_state = StashListState::Loaded;
                self.reported_stash_failure = None;
            }
            Err(error) => {
                let message = error.message().to_owned();
                self.stash_state = StashListState::Failed(message.clone());
                if self.reported_stash_failure.as_deref() != Some(message.as_str()) {
                    self.reported_stash_failure = Some(message.clone());
                    cx.emit(ChangesEvent::Error(message));
                }
            }
        }
        cx.emit(ChangesEvent::StashChanged);
        cx.notify();
    }

    /// Cancels the queued re-scans and drops whatever is in flight, leaving the list idle so
    /// the next switch back to the Changes tab reloads it; a rescan's previous report is
    /// dropped too, since the scan that would have replaced it never lands. The repository
    /// call already handed to its thread runs to completion, but its result is dropped by
    /// the generation check.
    pub fn cancel_scans(&mut self, cx: &mut Context<Self>) {
        self.status.pending = false;
        self.stash.pending = false;
        self.status.generation += 1;
        self.stash.generation += 1;
        if self.status.running && self.status_state != ChangesState::Idle {
            self.set_status_state(ChangesState::Idle);
            cx.emit(ChangesEvent::StatusChanged);
            cx.notify();
        }
    }
}
