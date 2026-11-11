//! The tick state behind the change list's boxes: which files are unticked, which carry only
//! some hunks, and which may not be acted on at all. Pure; the model keeps one and asks it.
//!
//! Files are ticked by default; unticks survive a rescan by path. Partial hunk ticks survive
//! one too, but only the stamps that still match a hunk of the file's fresh diff: a stamp that
//! matches nothing would make the guard refuse every later commit.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use rc_git_core::bstr::ByteSlice;
use rc_git_core::{BlobDiff, StatusReport};

use super::state::{RowTick, TickCount, TickSummary};
use crate::change_tree::ChangeNode;
use crate::guard_bridge::{CommitSelection, HunkStamp};
use crate::repository::ReadResult;

/// Every folder path's tick count, and the whole list's.
#[derive(Debug, Default)]
struct TickCounts {
    folders: HashMap<String, TickCount>,
    all: TickCount,
}

/// The tick state, with the per-folder counts built once per change to it or to the list.
#[derive(Debug, Default)]
pub(super) struct Ticks {
    unticked: HashSet<String>,
    /// Paths some status entry carries with U+FFFD replacements. The core reads a path
    /// argument as UTF-8, so such a path would act on a twin with the valid spelling, and the
    /// twin shares the string: every entry with one of these paths stays out of commit,
    /// stash and discard.
    inexact: HashSet<String>,
    /// Per partially ticked path, the ticked stamps; a set kept as a short list, since
    /// `HunkStamp` is not hashable and a file holds few hunks.
    hunks: HashMap<String, Vec<HunkStamp>>,
    /// Tick counts per folder path, so rendering a folder row reads one entry instead of
    /// walking every path beneath it.
    counts: OnceCell<TickCounts>,
}

/// Whether `a` and `b` hold the same stamps, ignoring order and repeats.
fn same_stamps(a: &[HunkStamp], b: &[HunkStamp]) -> bool {
    a.iter().all(|stamp| b.contains(stamp)) && b.iter().all(|stamp| a.contains(stamp))
}

/// The lossy spelling of every path in `report`, the key every tick is stored under.
fn paths(report: &StatusReport) -> impl Iterator<Item = String> + '_ {
    report
        .entries
        .iter()
        .map(|entry| entry.path.to_str_lossy().into_owned())
}

impl Ticks {
    /// Drops the cached counts; called on every change to the ticks or the list.
    pub(super) fn invalidate(&mut self) {
        self.counts = OnceCell::new();
    }

    pub(super) fn is_actionable(&self, path: &str) -> bool {
        !self.inexact.contains(path)
    }

    pub(super) fn is_ticked(&self, path: &str) -> bool {
        self.is_actionable(path) && !self.unticked.contains(path)
    }

    pub(super) fn is_partially_ticked(&self, path: &str) -> bool {
        self.is_ticked(path) && self.hunks.contains_key(path)
    }

    pub(super) fn is_hunk_ticked(&self, path: &str, stamp: HunkStamp) -> bool {
        if !self.is_ticked(path) {
            return false;
        }
        self.hunks
            .get(path)
            .is_none_or(|stamps| stamps.contains(&stamp))
    }

    pub(super) fn set_ticked(&mut self, paths: &[String], ticked: bool) {
        for path in paths {
            if ticked {
                self.unticked.remove(path);
            } else {
                self.unticked.insert(path.clone());
            }
            self.hunks.remove(path);
        }
        self.invalidate();
    }

    /// Toggles one hunk; `all` is the file's current full hunk list, so ticking the last
    /// missing hunk collapses back to a whole-file tick and unticking the last one unticks
    /// the file. Returns `false` when the path is not actionable and nothing changed.
    pub(super) fn toggle_hunk(&mut self, path: &str, stamp: HunkStamp, all: &[HunkStamp]) -> bool {
        if !self.is_actionable(path) {
            return false;
        }
        let mut stamps = match self.hunks.get(path) {
            Some(stamps) => stamps.clone(),
            None if self.is_ticked(path) => all.to_vec(),
            None => Vec::new(),
        };
        if let Some(index) = stamps.iter().position(|ticked| *ticked == stamp) {
            stamps.remove(index);
        } else {
            stamps.push(stamp);
        }
        self.unticked.remove(path);
        if stamps.is_empty() {
            self.unticked.insert(path.to_owned());
            self.hunks.remove(path);
        } else if same_stamps(&stamps, all) {
            self.hunks.remove(path);
        } else {
            self.hunks.insert(path.to_owned(), stamps);
        }
        self.invalidate();
        true
    }

    /// The paths with only some hunks ticked, whose diffs a rescan re-reads.
    pub(super) fn partial_paths(&self) -> HashSet<String> {
        self.hunks.keys().cloned().collect()
    }

    /// Takes in a fresh report: its inexact paths, and the ticks of paths that left it gone.
    pub(super) fn apply_report(&mut self, report: &StatusReport) {
        self.inexact = report
            .entries
            .iter()
            .filter(|entry| entry.path.to_str().is_err())
            .map(|entry| entry.path.to_str_lossy().into_owned())
            .collect();
        let current: HashSet<String> = paths(report).collect();
        self.unticked.retain(|path| current.contains(path));
        self.hunks.retain(|path, _| current.contains(path));
        self.invalidate();
    }

    /// Keeps only the ticked stamps that still name a current hunk. Ticking the rest of what
    /// is left collapses to a whole-file tick; nothing left unticks the file rather than
    /// widening the tick to hunks the user never picked. A diff that is not text has no
    /// hunks, and a failed read drops the tick the same way; its message is returned.
    pub(super) fn revalidate(
        &mut self,
        diffs: &HashMap<String, ReadResult<BlobDiff>>,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        for (path, read) in diffs {
            let Some(ticked) = self.hunks.get(path) else {
                continue;
            };
            let current: Vec<HunkStamp> = match read {
                Ok(BlobDiff::Text { hunks }) => hunks.iter().map(HunkStamp::from).collect(),
                Ok(BlobDiff::Binary { .. } | BlobDiff::TooLarge { .. }) => Vec::new(),
                Err(error) => {
                    errors.push(error.message().to_owned());
                    Vec::new()
                }
            };
            let kept: Vec<HunkStamp> = ticked
                .iter()
                .copied()
                .filter(|stamp| current.contains(stamp))
                .collect();
            if kept.is_empty() {
                self.unticked.insert(path.clone());
                self.hunks.remove(path);
            } else if same_stamps(&kept, &current) {
                self.hunks.remove(path);
            } else {
                self.hunks.insert(path.clone(), kept);
            }
        }
        self.invalidate();
        errors
    }

    fn tick_count(&self, path: &str) -> TickCount {
        let ticked = self.is_ticked(path);
        TickCount {
            total: 1,
            ticked: usize::from(ticked),
            whole: usize::from(ticked && !self.is_partially_ticked(path)),
            actionable: usize::from(self.is_actionable(path)),
        }
    }

    fn counts(&self, report: Option<&StatusReport>) -> &TickCounts {
        self.counts.get_or_init(|| {
            let mut counts = TickCounts::default();
            let Some(report) = report else {
                return counts;
            };
            for path in paths(report) {
                let one = self.tick_count(&path);
                counts.all.add(one);
                for (slash, _) in path.match_indices('/') {
                    counts
                        .folders
                        .entry(path[..slash].to_owned())
                        .or_default()
                        .add(one);
                }
            }
            counts
        })
    }

    pub(super) fn row_tick<E>(
        &self,
        node: &ChangeNode<E>,
        report: Option<&StatusReport>,
    ) -> RowTick {
        let count = if node.is_folder() {
            self.counts(report)
                .folders
                .get(&node.path)
                .copied()
                .unwrap_or_default()
        } else {
            self.tick_count(&node.path)
        };
        RowTick {
            state: count.state(),
            is_enabled: count.actionable > 0,
        }
    }

    pub(super) fn summary(&self, report: Option<&StatusReport>) -> TickSummary {
        let all = self.counts(report).all;
        TickSummary {
            text: if all.total == 0 {
                String::new()
            } else {
                format!("{} of {} selected", all.ticked, all.total)
            },
            has_selection: all.ticked > 0,
        }
    }

    pub(super) fn ticked_paths(&self, report: &StatusReport) -> Vec<String> {
        paths(report).filter(|path| self.is_ticked(path)).collect()
    }

    pub(super) fn selections(&self, report: &StatusReport) -> Vec<CommitSelection> {
        let mut selections = Vec::new();
        for path in paths(report) {
            if !self.is_ticked(&path) {
                continue;
            }
            match self.hunks.get(&path) {
                Some(stamps) => {
                    let mut hunks = stamps.clone();
                    hunks.sort_by_key(|stamp| stamp.old_start);
                    selections.push(CommitSelection::Hunks { path, hunks });
                }
                None => selections.push(CommitSelection::Whole { path }),
            }
        }
        selections
    }
}
