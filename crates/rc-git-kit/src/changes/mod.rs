//! Port of `ChangesModel.swift`, with `CommitAuthorModel` from `CommitIdentity.swift`: state
//! for the Changes tab. The flat change list, one file's diff against `HEAD`, the stash list,
//! the tick state that becomes a commit or stash selection, the list's grouping, and the
//! worktree watcher that keeps the list current. Reads only: guarded actions run through
//! [`crate::repository_actions`] in the window, which calls [`ChangesModel::refresh`] after.
//!
//! Working-tree data is never cached: disk is the one thing content addressing cannot cover.
//! Untracked files [`crate::hidden_files`] names are left out of the list while hidden files
//! are hidden, so they are never part of a commit or stash selection the user cannot see.
//!
//! A load driven by a watcher coalesces; it never queues. Each load keeps a `scanning` /
//! `pending` pair: a request made while one runs sets `pending` and returns, and the running
//! loop reads once more when it lands. Both are only touched on the UI thread between awaits,
//! so no request is lost and a burst of events costs one scan in flight plus one queued.

mod author;
mod scan;
mod state;
mod ticks;
mod wake;

use std::sync::Arc;

use gpui::{Context, EventEmitter, Task};
use rc_git_core::{Oid, StashEntry, StatusEntry, StatusReport};
use rc_git_guard::actions::ActionError;

pub use author::{AuthorChanged, CommitAuthorModel};
pub use state::{
    CLEAN_PLACEHOLDER, ChangesListNotice, ChangesState, RowTick, SCANNING_PLACEHOLDER,
    StashListState, TickState, TickSummary, WorktreeDiffState,
};

use crate::change_tree::{self, ChangeGrouping, ChangeNode};
use crate::guard_bridge::{CommitSelection, GuardFailure, HunkStamp, failure_response};
use crate::models::path_refusal;
use crate::platform::settings::SettingsError;
use crate::platform::stores::ChangeGroupingStore;
use crate::platform::watch::{WatcherFactory, WatcherStartError, WorktreeDirs};
use crate::repository::RepositoryReading;
use ticks::Ticks;
use wake::WakeFlag;

/// What changed, so a view redraws only that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesEvent {
    /// A scan moved the change list's state: the list itself may have changed.
    StatusChanged,
    /// Only the tick state moved: the list is the same, so the view redraws its tick boxes
    /// and summary without rebuilding the rows.
    TicksChanged,
    /// The diff pane's state moved.
    DiffChanged,
    /// The stash list or its state moved.
    StashChanged,
    /// The grouping changed: rebuild the rows from [`ChangesModel::nodes`].
    GroupingChanged,
    /// A read failed in a way the user should be told about beyond the inline state.
    Error(String),
}

/// One load stream's coalescing pair and generation.
#[derive(Debug, Default)]
struct Coalescer {
    /// A load is running.
    running: bool,
    /// Another was asked for while it ran.
    pending: bool,
    /// Bumped before every load; a load that finds it moved drops its result.
    generation: u64,
}

/// The working tree's watcher and the task that turns its events into rescans.
struct Watching {
    watcher: Box<dyn crate::platform::watch::Watcher>,
    _events: Task<()>,
}

/// State for the Changes tab of one repository window.
pub struct ChangesModel {
    repository: Arc<dyn RepositoryReading>,
    grouping_store: ChangeGroupingStore,
    grouping: ChangeGrouping,
    shows_hidden_files: bool,
    status_state: ChangesState,
    diff_state: WorktreeDiffState,
    stashes: Vec<StashEntry>,
    stash_state: StashListState,
    /// The stash the user picked, by commit rather than row: a push from elsewhere shifts
    /// every entry down a row, and the pick must stay on the stash the user saw.
    selected_stash: Option<Oid>,
    selected_path: Option<String>,
    expected_head: Option<Oid>,
    ticks: Ticks,
    status: Coalescer,
    stash: Coalescer,
    diff_generation: u64,
    /// The stash failure last reported. A watcher refresh re-reads the stash list on every
    /// event, so a failure that persists would otherwise raise one alert per event.
    reported_stash_failure: Option<String>,
    /// Whether the list is on screen, as the window last said, and whether the disk moved
    /// while it was not. A hidden list scans nothing: it remembers it is stale and pays one
    /// rescan when it is shown again.
    is_visible: bool,
    is_stale: bool,
    watching: Option<Watching>,
}

impl EventEmitter<ChangesEvent> for ChangesModel {}

impl ChangesModel {
    /// A model over `repository`, grouping its list as `grouping_store` remembers.
    #[must_use]
    pub fn new(
        repository: Arc<dyn RepositoryReading>,
        grouping_store: ChangeGroupingStore,
        shows_hidden_files: bool,
    ) -> Self {
        let grouping = grouping_store.grouping();
        Self {
            repository,
            grouping_store,
            grouping,
            shows_hidden_files,
            status_state: ChangesState::Idle,
            diff_state: WorktreeDiffState::Idle,
            stashes: Vec::new(),
            stash_state: StashListState::Loaded,
            selected_stash: None,
            selected_path: None,
            expected_head: None,
            ticks: Ticks::default(),
            status: Coalescer::default(),
            stash: Coalescer::default(),
            diff_generation: 0,
            reported_stash_failure: None,
            is_visible: false,
            is_stale: false,
            watching: None,
        }
    }

    /// The change list's state.
    #[must_use]
    pub fn status_state(&self) -> &ChangesState {
        &self.status_state
    }

    /// The diff pane's state.
    #[must_use]
    pub fn diff_state(&self) -> &WorktreeDiffState {
        &self.diff_state
    }

    /// The last stash list read; after a failed read it is the one before, and
    /// [`Self::stash_state`] says so.
    #[must_use]
    pub fn stashes(&self) -> &[StashEntry] {
        &self.stashes
    }

    /// Whether [`Self::stashes`] is current.
    #[must_use]
    pub fn stash_state(&self) -> &StashListState {
        &self.stash_state
    }

    /// The file whose diff the pane shows or loads.
    #[must_use]
    pub fn selected_path(&self) -> Option<&str> {
        self.selected_path.as_deref()
    }

    /// The commit `HEAD` pointed at when the list was loaded; `None` on an unborn branch.
    /// Guarded commits pass it back so a moved `HEAD` is refused instead of raced.
    #[must_use]
    pub fn expected_head(&self) -> Option<Oid> {
        self.expected_head
    }

    /// Whether the list is on screen, as [`Self::set_visible`] last said.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.is_visible
    }

    /// Whether the disk moved while the list was hidden.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.is_stale
    }

    /// Whether a status scan is running.
    #[must_use]
    pub fn is_scanning(&self) -> bool {
        self.status.running
    }

    /// Whether untracked hidden files are listed.
    #[must_use]
    pub fn shows_hidden_files(&self) -> bool {
        self.shows_hidden_files
    }

    /// Lists or leaves out untracked hidden files from the next scan on; the caller marks
    /// the list stale so the scan happens.
    pub fn set_shows_hidden_files(&mut self, shows: bool) {
        self.shows_hidden_files = shows;
    }

    fn set_status_state(&mut self, state: ChangesState) {
        self.status_state = state;
        self.ticks.invalidate();
    }

    fn report(&self) -> Option<&StatusReport> {
        match &self.status_state {
            ChangesState::Loaded(report) => Some(report),
            ChangesState::Idle | ChangesState::Loading | ChangesState::Failed(_) => None,
        }
    }

    /// Reloads the change list, the stash list, and the captured `HEAD`. The task ends when
    /// both loops this call joined have finished.
    pub fn refresh(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let status = self.load_status(cx);
        let stash = self.load_stashes(cx);
        cx.spawn(async move |_, _| {
            status.await;
            stash.await;
        })
    }

    /// Answers a guarded action that failed. A stale repository reloads the change and stash
    /// lists before the task resolves, so the notice the window shows sits over what is there
    /// now; any other failure leaves the lists alone.
    pub fn recover(&mut self, error: &ActionError, cx: &mut Context<Self>) -> Task<GuardFailure> {
        let response = failure_response(error);
        match response {
            GuardFailure::Stale { .. } => {
                let reload = self.refresh(cx);
                cx.spawn(async move |_, _| {
                    reload.await;
                    response
                })
            }
            GuardFailure::Failed { .. } => Task::ready(response),
        }
    }

    /// The working tree, the refs or the hidden-files setting changed. A visible list
    /// rescans now; a hidden one only remembers, so a burst of events while it is off screen
    /// costs no scan at all.
    pub fn mark_stale(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if !self.is_visible {
            self.is_stale = true;
            return Task::ready(());
        }
        self.refresh(cx)
    }

    /// Only the working tree changed. A visible list rescans the change list alone: the stash
    /// list lives under `.git`, where the ref watcher reports its moves through
    /// [`Self::mark_stale`].
    pub fn mark_worktree_stale(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if !self.is_visible {
            self.is_stale = true;
            return Task::ready(());
        }
        self.load_status(cx)
    }

    /// Shows or hides the list. Showing it rescans when it went stale while hidden or has
    /// never loaded; hiding it cancels the running scan, which is work for no one.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) -> Task<()> {
        self.is_visible = visible;
        if !visible {
            self.cancel_scans(cx);
            return Task::ready(());
        }
        let needs_load = self.is_stale || self.status_state == ChangesState::Idle;
        self.is_stale = false;
        if needs_load {
            self.refresh(cx)
        } else {
            Task::ready(())
        }
    }

    /// Starts watching the working tree described by `dirs` with a watcher from `factory`;
    /// each relevant event marks the list stale through [`Self::mark_worktree_stale`]. A
    /// second call replaces the first watcher.
    ///
    /// # Errors
    /// [`WatcherStartError`] when the watcher's stream could not start: changes on disk will
    /// then not refresh the list.
    pub fn watch_worktree(
        &mut self,
        factory: &dyn WatcherFactory,
        dirs: &WorktreeDirs,
        cx: &mut Context<Self>,
    ) -> Result<(), WatcherStartError> {
        self.stop_watching();
        let flag = WakeFlag::default();
        let raise = flag.clone();
        let watcher = factory.worktree_watcher(dirs, Arc::new(move || raise.raise()));
        let events = cx.spawn(async move |this, cx| {
            loop {
                flag.raised().await;
                let rescan = this.update(cx, Self::mark_worktree_stale);
                let Ok(rescan) = rescan else { break };
                rescan.detach();
            }
        });
        watcher.start()?;
        if let Some(report) = self.report() {
            prune(watcher.as_ref(), report);
        }
        self.watching = Some(Watching {
            watcher,
            _events: events,
        });
        Ok(())
    }

    /// Stops the worktree watcher, if one runs.
    pub fn stop_watching(&mut self) {
        if let Some(watching) = self.watching.take() {
            watching.watcher.stop();
        }
    }

    /// The list's grouping.
    #[must_use]
    pub fn grouping(&self) -> ChangeGrouping {
        self.grouping
    }

    /// Switches the grouping and remembers it.
    ///
    /// # Errors
    /// [`SettingsError`] when it could not be saved; the list still switches.
    pub fn set_grouping(
        &mut self,
        grouping: ChangeGrouping,
        cx: &mut Context<Self>,
    ) -> Result<(), SettingsError> {
        if grouping == self.grouping {
            return Ok(());
        }
        self.grouping = grouping;
        cx.emit(ChangesEvent::GroupingChanged);
        cx.notify();
        self.grouping_store.set_grouping(grouping)
    }

    /// The list's rows in its grouping, or none while no report is loaded. Rebuilt on each
    /// call: the view asks once per [`ChangesEvent::StatusChanged`] or
    /// [`ChangesEvent::GroupingChanged`].
    #[must_use]
    pub fn nodes(&self) -> Vec<ChangeNode<StatusEntry>> {
        self.report().map_or_else(Vec::new, |report| {
            change_tree::nodes(&report.entries, self.grouping)
        })
    }

    /// The row of the picked stash in [`Self::stashes`], or `None` when none is picked or the
    /// last read failed, since the view then shows the error in place of the list.
    #[must_use]
    pub fn selected_stash_index(&self) -> Option<usize> {
        if self.stash_state != StashListState::Loaded {
            return None;
        }
        let picked = self.selected_stash?;
        self.stashes.iter().position(|entry| entry.oid == picked)
    }

    /// Picks the stash at `index` in [`Self::stashes`]; `None` or a row past the list clears
    /// the pick.
    pub fn select_stash(&mut self, index: Option<usize>) {
        self.selected_stash = index
            .and_then(|index| self.stashes.get(index))
            .map(|entry| entry.oid);
    }

    // Tick state.

    /// Whether `path` may be committed, stashed or discarded: `false` when a status entry
    /// carries it with U+FFFD replacements.
    #[must_use]
    pub fn is_actionable(&self, path: &str) -> bool {
        self.ticks.is_actionable(path)
    }

    /// The refusal to show for discarding `paths`, or `None` when every one is actionable.
    #[must_use]
    pub fn discard_refusal(&self, paths: &[String]) -> Option<String> {
        let path = paths.iter().find(|path| !self.is_actionable(path))?;
        Some(path_refusal("discard", path))
    }

    /// Whether the whole file is in the selection. Partially ticked files count as ticked;
    /// a path that is not actionable never is.
    #[must_use]
    pub fn is_ticked(&self, path: &str) -> bool {
        self.ticks.is_ticked(path)
    }

    /// Whether only some hunks of `path` are selected.
    #[must_use]
    pub fn is_partially_ticked(&self, path: &str) -> bool {
        self.ticks.is_partially_ticked(path)
    }

    /// Ticks or unticks whole files. A folder row hands over every path beneath it, so one
    /// click moves them together and renders once.
    pub fn set_ticked(&mut self, paths: &[String], ticked: bool, cx: &mut Context<Self>) {
        self.ticks.set_ticked(paths, ticked);
        cx.emit(ChangesEvent::TicksChanged);
        cx.notify();
    }

    /// Whether one hunk of `path` is in the selection. With no partial state, every hunk of a
    /// ticked file is.
    #[must_use]
    pub fn is_hunk_ticked(&self, path: &str, stamp: HunkStamp) -> bool {
        self.ticks.is_hunk_ticked(path, stamp)
    }

    /// Toggles one hunk. `all` is the file's current full hunk list, so ticking the last
    /// missing hunk collapses back to a whole-file tick and unticking the last one unticks
    /// the file.
    pub fn toggle_hunk(
        &mut self,
        path: &str,
        stamp: HunkStamp,
        all: &[HunkStamp],
        cx: &mut Context<Self>,
    ) {
        if self.ticks.toggle_hunk(path, stamp, all) {
            cx.emit(ChangesEvent::TicksChanged);
            cx.notify();
        }
    }

    /// The tick box of one change list row: a file's own state, or the sum of every file
    /// beneath a folder.
    #[must_use]
    pub fn row_tick(&self, node: &ChangeNode<StatusEntry>) -> RowTick {
        self.ticks.row_tick(node, self.report())
    }

    /// How many files are in the selection, partially ticked ones included.
    #[must_use]
    pub fn tick_summary(&self) -> TickSummary {
        self.ticks.summary(self.report())
    }

    /// The ticked paths, for stash and discard.
    #[must_use]
    pub fn ticked_paths(&self) -> Vec<String> {
        self.report()
            .map_or_else(Vec::new, |report| self.ticks.ticked_paths(report))
    }

    /// The commit selection: whole files, except where only some hunks are ticked.
    #[must_use]
    pub fn selections(&self) -> Vec<CommitSelection> {
        self.report()
            .map_or_else(Vec::new, |report| self.ticks.selections(report))
    }
}

/// Hands a fresh report's pruned directories to the worktree watcher, so events below them
/// are dropped.
fn prune(watcher: &dyn crate::platform::watch::Watcher, report: &StatusReport) {
    use rc_git_core::bstr::ByteSlice;
    let lossy = |dirs: &[rc_git_core::bstr::BString]| -> Vec<String> {
        dirs.iter()
            .map(|dir| dir.to_str_lossy().into_owned())
            .collect()
    };
    watcher.set_pruned_dirs(&lossy(&report.ignored_dirs), &lossy(&report.nested_repos));
}
