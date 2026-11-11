//! The value types of `ChangesModel.swift`: the change list's, diff pane's and stash list's
//! load states, the notice the list shows besides its rows, and the tick boxes.

use rc_git_core::{BlobDiff, StatusEntry, StatusReport};

/// What the list shows while no scan has landed: the walk can take seconds on a large
/// checkout, and "No changes" in that time reads as a clean tree.
pub const SCANNING_PLACEHOLDER: &str = "Scanning the working tree…";
/// What the list shows over a loaded report with no entries.
pub const CLEAN_PLACEHOLDER: &str = "No changes";

/// The change list's load state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ChangesState {
    /// Nothing loaded, and nothing loading.
    #[default]
    Idle,
    /// A first scan is running; a rescan keeps the last [`ChangesState::Loaded`] instead.
    Loading,
    /// The last scan's report.
    Loaded(StatusReport),
    /// The scan failed; the message says why.
    Failed(String),
}

/// What the change list shows besides its rows: the line in place of them, and whether the
/// rows on screen are the state's own.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChangesListNotice {
    /// The line shown over the list; `None` when the rows stand alone.
    pub placeholder: Option<String>,
    /// Whether the rows on screen came from the current state; `false` while they are left
    /// over from a report a scan that has not landed yet will replace.
    pub is_current: bool,
}

impl ChangesState {
    /// The rows this state replaces the list with, or `None` when it carries none and the rows
    /// on screen stay as they are. Only `None` spares the view a rebuild, which on a large
    /// change list is the costliest step of rendering a status change.
    #[must_use]
    pub fn listed_entries(&self) -> Option<&[StatusEntry]> {
        match self {
            Self::Idle | Self::Loading => None,
            Self::Loaded(report) => Some(&report.entries),
            Self::Failed(_) => Some(&[]),
        }
    }

    /// The list's notice in this state. Only a loaded report with no entries says the tree
    /// is clean; before one lands, the list says it is scanning, and rows left from an earlier
    /// report stay but are not current. `showing_rows` says whether the list still holds rows
    /// from an earlier report.
    #[must_use]
    pub fn notice(&self, showing_rows: bool) -> ChangesListNotice {
        match self {
            Self::Idle | Self::Loading => ChangesListNotice {
                placeholder: (!showing_rows).then(|| SCANNING_PLACEHOLDER.to_owned()),
                is_current: false,
            },
            Self::Loaded(report) => ChangesListNotice {
                placeholder: report
                    .entries
                    .is_empty()
                    .then(|| CLEAN_PLACEHOLDER.to_owned()),
                is_current: true,
            },
            Self::Failed(message) => ChangesListNotice {
                placeholder: Some(message.clone()),
                is_current: true,
            },
        }
    }
}

/// The worktree diff pane's load state, keyed by the path it shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorktreeDiffState {
    /// No file picked.
    #[default]
    Idle,
    /// The file's diff is loading.
    Loading(String),
    /// The file and its diff against `HEAD`.
    Loaded(String, BlobDiff),
    /// The file and why its diff failed.
    Failed(String, String),
}

/// The stash list's load state. A failed read keeps the last list in the model's stashes, so
/// the state is what tells the view that list is no longer current.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum StashListState {
    /// The list is the last read's.
    #[default]
    Loaded,
    /// The last read failed; the message says why.
    Failed(String),
}

/// A change list row's tick box. A file with only some hunks ticked is `Mixed`, like a folder
/// with only some files ticked: the row never claims more than the commit will take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TickState {
    /// Nothing at or below the row is in the selection.
    Off,
    /// Some of it is.
    Mixed,
    /// All of it is, wholly.
    On,
}

/// What a change list row's tick box shows, and whether it takes a click: a row whose every
/// path is inexact has nothing it could tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowTick {
    /// The box's state.
    pub state: TickState,
    /// Whether a click can change it.
    pub is_enabled: bool,
}

/// The line under the change list and whether Commit and Stash have anything to take.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TickSummary {
    /// "2 of 3 selected", or empty when the list is.
    pub text: String,
    /// Whether any file is in the selection.
    pub has_selection: bool,
}

/// Files at or below one row: how many there are, how many are in the selection at all, how
/// many wholly, and how many could be.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TickCount {
    pub(super) total: usize,
    pub(super) ticked: usize,
    pub(super) whole: usize,
    pub(super) actionable: usize,
}

impl TickCount {
    pub(super) fn state(self) -> TickState {
        if self.ticked == 0 {
            return TickState::Off;
        }
        if self.whole == self.total {
            TickState::On
        } else {
            TickState::Mixed
        }
    }

    pub(super) fn add(&mut self, other: Self) {
        self.total += other.total;
        self.ticked += other.ticked;
        self.whole += other.whole;
        self.actionable += other.actionable;
    }
}
