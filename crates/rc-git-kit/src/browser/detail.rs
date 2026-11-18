//! The commit detail beside the history: one commit's changed files, its full message and
//! ref badges, and the line diff of one changed file.

use gpui::{Context, Task};
use rc_git_core::{CommitDetail, CommitSummary, DEFAULT_BLOB_LIMIT, Oid, RefEntry, TreeChange};

use super::{BrowserEvent, BrowserModel, call, with};
use crate::blocking::blocking;
use crate::models::{FileDiffState, new_blob, old_blob};
use crate::repository::ReadResult;

/// Lines of context around each diff hunk.
const DIFF_CONTEXT: u32 = 3;

/// What the commit detail header shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitDetailState {
    /// No commit picked.
    Empty,
    /// The commit's detail is loading.
    Loading(CommitSummary),
    /// The commit's detail and the refs pointing at it.
    Loaded {
        /// The detail.
        detail: CommitDetail,
        /// Refs whose target is the commit, for the header's badges.
        refs: Vec<RefEntry>,
    },
    /// The commit and why its detail failed.
    Failed(CommitSummary, String),
}

impl CommitDetailState {
    /// The diff pane's line while no file diff is shown. Only a loaded commit with no changed
    /// files says "No files changed"; a load in flight or a failed one never does.
    #[must_use]
    pub fn empty_diff_placeholder(&self, change_count: usize) -> &'static str {
        match self {
            Self::Empty | Self::Failed(..) => "",
            Self::Loading(_) => "Loading…",
            Self::Loaded { .. } if change_count == 0 => "No files changed",
            Self::Loaded { .. } => "Select a file",
        }
    }

    /// The open commit, whatever its detail's state; `None` when no commit is picked.
    #[must_use]
    pub fn summary(&self) -> Option<&CommitSummary> {
        match self {
            Self::Empty => None,
            Self::Loading(summary) | Self::Failed(summary, _) => Some(summary),
            Self::Loaded { detail, .. } => Some(&detail.summary),
        }
    }
}

impl BrowserModel {
    /// Closes the commit detail, so the view can dismiss its drawer.
    pub fn close_commit_detail(&mut self, cx: &mut Context<Self>) {
        self.clear_commit_detail(cx);
        self.note_location(cx);
    }

    pub(super) fn clear_commit_detail(&mut self, cx: &mut Context<Self>) {
        self.generations.diff += 1;
        if self.diff_commit.take().is_some() {
            self.commit_changes.clear();
            self.file_diff = FileDiffState::Empty;
            self.commit_detail = CommitDetailState::Empty;
            Self::changed(BrowserEvent::DiffChanged, cx);
        }
    }

    /// Loads the files `commit` changed against its first parent, and its full detail. A root
    /// commit diffs against the empty tree. The first changed file's diff loads with it.
    pub fn load_commit_changes(
        &mut self,
        commit: CommitSummary,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(());
        };
        self.generations.diff += 1;
        let generation = self.generations.diff;
        let parent = commit.parents.first().copied();
        let target = commit.oid;
        self.diff_commit = Some(commit.clone());
        self.commit_changes.clear();
        self.file_diff = FileDiffState::Empty;
        self.commit_detail = CommitDetailState::Loading(commit.clone());
        Self::changed(BrowserEvent::DiffChanged, cx);
        self.note_location(cx);
        let work = blocking(cx.background_executor(), move || {
            let changes = repository.diff_trees(parent, target)?;
            let detail = repository.commit_details(target)?;
            Ok((changes, detail))
        });
        Self::run(cx, (), async move |this, cx| {
            let result = work.await;
            let first = with(&this, cx, |model, cx| {
                model.apply_commit_changes(generation, commit, result, cx)
            })
            .flatten();
            if let Some(first) = first {
                call(&this, cx, |model, cx| model.load_file_diff(first, cx)).await;
            }
        })
    }

    /// Applies a commit's changes and detail; the first change, whose diff loads next.
    fn apply_commit_changes(
        &mut self,
        generation: u64,
        commit: CommitSummary,
        result: ReadResult<(Vec<TreeChange>, CommitDetail)>,
        cx: &mut Context<Self>,
    ) -> Option<TreeChange> {
        if generation != self.generations.diff {
            return None;
        }
        match result {
            Ok((changes, detail)) => {
                let refs = self.refs_pointing(commit.oid);
                self.commit_detail = CommitDetailState::Loaded { detail, refs };
                let first = changes.first().cloned();
                self.commit_changes = changes;
                if first.is_none() {
                    Self::changed(BrowserEvent::DiffChanged, cx);
                }
                first
            }
            Err(error) => {
                self.commit_detail = CommitDetailState::Failed(commit, error.message().to_owned());
                Self::changed(BrowserEvent::DiffChanged, cx);
                Self::fail(error.message(), cx);
                None
            }
        }
    }

    /// Every ref whose target is `oid`, for the detail header's badges.
    #[must_use]
    pub fn refs_pointing(&self, oid: Oid) -> Vec<RefEntry> {
        let mut pointing = Vec::new();
        for entry in &self.refs.refs {
            if entry.target == oid {
                pointing.push(entry.clone());
            }
        }
        pointing
    }

    /// Shows the detail of a commit reached by id: a parent link, a revision jump, or a stop
    /// of the back/forward list. A commit picked during the read wins.
    pub fn jump_to_commit(&mut self, oid: Oid, cx: &mut Context<Self>) -> Task<()> {
        let generation = self.generations.diff;
        let read = self.read_commit(oid, cx);
        Self::run(cx, (), async move |this, cx| {
            let Some(summary) = read.await else {
                return;
            };
            let current = with(&this, cx, |model, _| generation == model.generations.diff);
            if current != Some(true) {
                return;
            }
            call(&this, cx, |model, cx| {
                model.load_commit_changes(summary, cx)
            })
            .await;
        })
    }

    /// One commit's summary, read off the UI thread. A commit that cannot be read is reported
    /// and answers `None`.
    pub(super) fn read_commit(
        &mut self,
        oid: Oid,
        cx: &mut Context<Self>,
    ) -> Task<Option<CommitSummary>> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(None);
        };
        let read = blocking(cx.background_executor(), move || repository.commit(oid));
        Self::run(cx, None, async move |this, cx| match read.await {
            Ok(summary) => Some(summary),
            Err(error) => {
                with(&this, cx, |_, cx| Self::fail(error.message(), cx));
                None
            }
        })
    }

    /// Loads the line diff of one changed file. A gitlink side has no blob in this repository:
    /// a submodule change settles without a read, and a file that became a submodule, or the
    /// reverse, diffs its file side as a deletion or an addition.
    pub fn load_file_diff(&mut self, change: TreeChange, cx: &mut Context<Self>) -> Task<()> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(());
        };
        self.generations.diff += 1;
        let generation = self.generations.diff;
        let old = old_blob(&change);
        let new = new_blob(&change);
        if old.is_none() && new.is_none() {
            self.file_diff = FileDiffState::Submodule(change);
            Self::changed(BrowserEvent::DiffChanged, cx);
            return Task::ready(());
        }
        self.file_diff = FileDiffState::Loading(change.clone());
        Self::changed(BrowserEvent::DiffChanged, cx);
        let work = blocking(cx.background_executor(), move || {
            repository.diff_blobs(old, new, DEFAULT_BLOB_LIMIT, DIFF_CONTEXT)
        });
        Self::run(cx, (), async move |this, cx| {
            let result = work.await;
            with(&this, cx, |model, cx| {
                if generation != model.generations.diff {
                    return;
                }
                model.file_diff = match result {
                    Ok(diff) => FileDiffState::Loaded(change, diff),
                    Err(error) => FileDiffState::Failed(change, error.message().to_owned()),
                };
                Self::changed(BrowserEvent::DiffChanged, cx);
            });
        })
    }
}
