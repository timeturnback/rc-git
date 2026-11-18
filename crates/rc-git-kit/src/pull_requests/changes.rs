//! A pull request's changes, read from local objects: the files that differ between the merge
//! base of (base, head) and the head, GitHub's three-dot view, and one file's diff at a time.
//!
//! The diff always belongs to one [`PullRequestStamp`], the read the user saw: its head is the
//! commit diffed, so a pull request that moved can never show another commit's files under its
//! name. When the head is not in the object database the state waits for the user to fetch it
//! ([`ChangesState::fetch`]); nothing here starts a fetch on its own, since a pull request's
//! size is whatever its author made it. A fetch that finds the remote's head is another commit
//! than the stamp's answers [`HeadFetched::Moved`] and writes nothing; the tab then reads the
//! pull request again.
//!
//! One load runs at a time (a read of the local state, or a fetch) under a generation and the
//! stamp it was asked for, so an answer for an older load or another stamp is dropped, and so
//! is a file diff for an older selection.

use rc_git_core::{BlobDiff, MergeBase, Oid, TreeChange};

use super::PullRequestStamp;
use crate::models::{FileDiffState, new_blob, old_blob};
use crate::repository::RepositoryReading;

/// The most commits a merge-base walk reads before it answers that it could not tell.
pub const MERGE_BASE_SCAN: usize = 50_000;

/// What the repository holds for a pull request's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesRead {
    /// The head commit is not in the object database: it has to be fetched first.
    HeadMissing,
    /// The repository keeps no copy of the base branch: `tracking` is where the remote's
    /// refspecs would put one (`None` when they map none).
    NoBaseCopy {
        /// The base branch, as the host named it.
        branch: String,
        /// The remote-tracking ref that would hold it.
        tracking: Option<String>,
    },
    /// The walk from the base's copy and the head found no merge base.
    NoMergeBase {
        /// The remote-tracking ref of the base.
        tracking: String,
        /// Whether the walk stopped at its limit or a shallow edge rather than running out of
        /// history.
        incomplete: bool,
        /// Commits read.
        scanned: usize,
    },
    /// The changed files between the merge base and the head.
    Files {
        /// The merge base.
        base: Oid,
        /// The remote-tracking ref of the base.
        tracking: String,
        /// The changed files, sorted by path.
        files: Vec<TreeChange>,
    },
}

/// Reads what the repository holds for the diff of `stamp`'s head against its base, whose
/// local copy is `tracking` (as `rc_git_guard`'s `pull_request_base_ref` names it). Local only.
///
/// # Errors
/// The read's failure, as a sentence.
pub fn read_changes(
    repository: &dyn RepositoryReading,
    stamp: &PullRequestStamp,
    tracking: Option<String>,
) -> Result<ChangesRead, String> {
    let head = stamp.head_oid;
    if !repository.has_object(head) {
        return Ok(ChangesRead::HeadMissing);
    }
    let branch = stamp.base_ref.clone();
    let Some(tracking) = tracking else {
        return Ok(ChangesRead::NoBaseCopy {
            branch,
            tracking: None,
        });
    };
    let Ok(base_tip) = repository.resolve(&tracking) else {
        return Ok(ChangesRead::NoBaseCopy {
            branch,
            tracking: Some(tracking),
        });
    };
    let found = repository
        .merge_base(base_tip, head, MERGE_BASE_SCAN)
        .map_err(|error| error.message().to_owned())?;
    let (incomplete, scanned) = match found {
        MergeBase::Found { base, .. } => {
            let files = repository
                .diff_trees(Some(base), head)
                .map_err(|error| error.message().to_owned())?;
            return Ok(ChangesRead::Files {
                base,
                tracking,
                files,
            });
        }
        MergeBase::Unrelated { scanned } => (false, scanned),
        MergeBase::Incomplete { scanned } => (true, scanned),
    };
    Ok(ChangesRead::NoMergeBase {
        tracking,
        incomplete,
        scanned,
    })
}

/// What a fetch of the pull request's head did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadFetched {
    /// The head commit is in the object database now.
    Fetched,
    /// The remote's head is `found`, not the stamp's: the pull request moved after it was
    /// read. Nothing was written.
    Moved {
        /// The head the remote advertised.
        found: Oid,
    },
}

/// Why a fetch of the head gave no answer. Each failure carries what to do, as a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchProblem {
    /// The user stopped it.
    Cancelled,
    /// The host wants a secret that is not saved for it, or refused the saved one.
    NeedsSecret(String),
    /// The ssh host's key is not approved yet.
    UnknownHostKey(String),
    /// Any other failure.
    Failed(String),
}

impl FetchProblem {
    /// What to tell the user; `None` for a cancel.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Cancelled => None,
            Self::NeedsSecret(message) | Self::UnknownHostKey(message) | Self::Failed(message) => {
                Some(message)
            }
        }
    }
}

/// Where a pull request's changes stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesPhase {
    /// Reading what the repository holds.
    Reading,
    /// The head is not local: Fetch fetches it. `problem` says why the last fetch failed.
    NeedsFetch {
        /// The last fetch's failure.
        problem: Option<String>,
    },
    /// The head is being fetched; Stop cancels.
    Fetching,
    /// The remote's head moved past the stamp's; the pull request is being read again.
    Moved {
        /// The head the remote advertised.
        found: Oid,
    },
    /// No local copy of the base branch.
    NoBaseCopy {
        /// The base branch.
        branch: String,
        /// Where its copy would be.
        tracking: Option<String>,
    },
    /// No merge base found.
    NoMergeBase {
        /// The base's copy.
        tracking: String,
        /// Whether the walk gave up rather than ran out of history.
        incomplete: bool,
        /// Commits read.
        scanned: usize,
    },
    /// The changed files.
    Ready {
        /// The merge base.
        base: Oid,
        /// The base's copy.
        tracking: String,
        /// The files, sorted by path.
        files: Vec<TreeChange>,
    },
    /// The local read failed.
    Failed(String),
}

/// A load to run for the changes, under its generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesLoad {
    /// Read what the repository holds for the stamp; answer with
    /// [`ChangesState::finish_read`].
    Read {
        /// The load's generation.
        generation: u64,
        /// What to read for.
        stamp: PullRequestStamp,
    },
    /// Fetch the stamp's head; answer with [`ChangesState::finish_fetch`].
    Fetch {
        /// The load's generation.
        generation: u64,
        /// What to fetch.
        stamp: PullRequestStamp,
    },
}

/// One file's diff to read, under its generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLoad {
    /// Answer [`ChangesState::finish_file`] with it.
    pub generation: u64,
    /// The change to diff.
    pub change: TreeChange,
}

/// One pull request's changes, for the stamp they were read for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesState {
    stamp: PullRequestStamp,
    phase: ChangesPhase,
    generation: u64,
    selected: Option<usize>,
    file: FileDiffState,
    file_generation: u64,
}

impl ChangesState {
    /// The changes of `stamp`, about to be read: answer the returned load.
    #[must_use]
    pub fn new(stamp: PullRequestStamp) -> (Self, ChangesLoad) {
        let mut state = Self {
            stamp,
            phase: ChangesPhase::Reading,
            generation: 0,
            selected: None,
            file: FileDiffState::Empty,
            file_generation: 0,
        };
        let load = state.read();
        (state, load)
    }

    /// The read the changes belong to: its head is the commit diffed.
    #[must_use]
    pub fn stamp(&self) -> &PullRequestStamp {
        &self.stamp
    }

    /// Where they stand.
    #[must_use]
    pub fn phase(&self) -> &ChangesPhase {
        &self.phase
    }

    /// The selected file's index in the list.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The selected file's diff.
    #[must_use]
    pub fn file_diff(&self) -> &FileDiffState {
        &self.file
    }

    /// Whether a fetch runs, so Stop shows.
    #[must_use]
    pub fn is_fetching(&self) -> bool {
        self.phase == ChangesPhase::Fetching
    }

    fn read(&mut self) -> ChangesLoad {
        self.generation += 1;
        self.phase = ChangesPhase::Reading;
        ChangesLoad::Read {
            generation: self.generation,
            stamp: self.stamp.clone(),
        }
    }

    /// Starts the fetch the user asked for: only while the head is missing, or after the
    /// remote's head was found to have moved. Anything else answers `None`.
    pub fn fetch(&mut self) -> Option<ChangesLoad> {
        match self.phase {
            ChangesPhase::NeedsFetch { .. } | ChangesPhase::Moved { .. } => {}
            ChangesPhase::Reading
            | ChangesPhase::Fetching
            | ChangesPhase::NoBaseCopy { .. }
            | ChangesPhase::NoMergeBase { .. }
            | ChangesPhase::Ready { .. }
            | ChangesPhase::Failed(_) => return None,
        }
        self.generation += 1;
        self.phase = ChangesPhase::Fetching;
        Some(ChangesLoad::Fetch {
            generation: self.generation,
            stamp: self.stamp.clone(),
        })
    }

    /// Stops waiting for the running fetch: its answer is dropped and Fetch shows again.
    /// Answers whether a fetch was running.
    pub fn cancel_fetch(&mut self) -> bool {
        if !self.is_fetching() {
            return false;
        }
        self.generation += 1;
        self.phase = ChangesPhase::NeedsFetch { problem: None };
        true
    }

    /// Reads the local state again (Look Again after fetching the base).
    pub fn reread(&mut self) -> Option<ChangesLoad> {
        if self.is_fetching() {
            return None;
        }
        self.selected = None;
        self.file = FileDiffState::Empty;
        Some(self.read())
    }

    /// Takes the answer of read `generation` asked for `stamp`; an answer for any other
    /// generation or stamp is dropped.
    pub fn finish_read(
        &mut self,
        generation: u64,
        stamp: &PullRequestStamp,
        answer: Result<ChangesRead, String>,
    ) {
        let current = generation == self.generation && *stamp == self.stamp;
        if !current || self.phase != ChangesPhase::Reading {
            return;
        }
        self.phase = match answer {
            Ok(ChangesRead::HeadMissing) => ChangesPhase::NeedsFetch { problem: None },
            Ok(ChangesRead::NoBaseCopy { branch, tracking }) => {
                ChangesPhase::NoBaseCopy { branch, tracking }
            }
            Ok(ChangesRead::NoMergeBase {
                tracking,
                incomplete,
                scanned,
            }) => ChangesPhase::NoMergeBase {
                tracking,
                incomplete,
                scanned,
            },
            Ok(ChangesRead::Files {
                base,
                tracking,
                files,
            }) => ChangesPhase::Ready {
                base,
                tracking,
                files,
            },
            Err(message) => ChangesPhase::Failed(message),
        };
    }

    /// Takes the answer of fetch `generation` asked for `stamp`; an answer for any other
    /// generation or stamp is dropped. A fetched head answers the read to run next; a moved one
    /// leaves the state in [`ChangesPhase::Moved`] with nothing to diff.
    pub fn finish_fetch(
        &mut self,
        generation: u64,
        stamp: &PullRequestStamp,
        answer: Result<HeadFetched, FetchProblem>,
    ) -> Option<ChangesLoad> {
        let current = generation == self.generation && *stamp == self.stamp;
        if !current || !self.is_fetching() {
            return None;
        }
        match answer {
            Ok(HeadFetched::Fetched) => return Some(self.read()),
            Ok(HeadFetched::Moved { found }) => self.phase = ChangesPhase::Moved { found },
            Err(problem) => {
                self.phase = ChangesPhase::NeedsFetch {
                    problem: problem.message().map(str::to_owned),
                };
            }
        }
        None
    }

    /// Selects file `index` of the ready list and answers the diff to read, `None` when there
    /// is no such file or nothing to read (a submodule change settles at once).
    pub fn select(&mut self, index: usize) -> Option<FileLoad> {
        let ChangesPhase::Ready { files, .. } = &self.phase else {
            return None;
        };
        let change = files.get(index)?.clone();
        self.selected = Some(index);
        self.file_generation += 1;
        if old_blob(&change).is_none() && new_blob(&change).is_none() {
            self.file = FileDiffState::Submodule(change);
            return None;
        }
        self.file = FileDiffState::Loading(change.clone());
        Some(FileLoad {
            generation: self.file_generation,
            change,
        })
    }

    /// Takes the diff `load` asked for; an answer for an older selection is dropped.
    pub fn finish_file(&mut self, load: &FileLoad, answer: Result<BlobDiff, String>) {
        if load.generation != self.file_generation {
            return;
        }
        let FileDiffState::Loading(change) = &self.file else {
            return;
        };
        if *change != load.change {
            return;
        }
        let change = change.clone();
        self.file = match answer {
            Ok(diff) => FileDiffState::Loaded(change, diff),
            Err(message) => FileDiffState::Failed(change, message),
        };
    }
}
