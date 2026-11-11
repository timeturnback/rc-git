//! Blame for the file in the file pane, at the browsed revision, walking at most two years
//! back from the tip.

use std::collections::HashMap;

use gpui::{Context, Task};
use rc_git_core::{Blame, CommitSummary, Oid, WorktreeStatus};

use super::{BlameTarget, BrowserEvent, BrowserModel, OpenedFileSource, with};
use crate::blocking::blocking;
use crate::models::{BlameSpan, BlameState, path_refusal};
use crate::repository::{ReadResult, RepositoryError, RepositoryReading};

/// How far back blame walks: two years (730 days), in seconds.
pub const BLAME_WINDOW: i64 = 730 * 86_400;

/// The `since` floor blame walks to from a tip committed at `tip_time` (seconds since the Unix
/// epoch): [`BLAME_WINDOW`] before it.
#[must_use]
pub fn blame_floor(tip_time: i64) -> i64 {
    tip_time - BLAME_WINDOW
}

/// The blame panel's title: the path, marked as `HEAD`'s version when the working tree stood
/// in for a revision.
#[must_use]
pub fn blame_title(path: &str, at_head: bool) -> String {
    if at_head {
        format!("Blame \u{2014} {path} at HEAD")
    } else {
        format!("Blame \u{2014} {path}")
    }
}

/// The blame panel's title while the walk for `path` runs.
#[must_use]
pub fn blaming_title(path: &str) -> String {
    format!("Blaming {path}\u{2026}")
}

/// Why Show Blame cannot run, checked in order: no repository, no file with a known path, no
/// revision to blame it at, an untracked file, a path that is not valid UTF-8. `None` when it
/// can run.
#[must_use]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "five independent conditions, each checked in turn; the order is the API"
)]
pub fn blame_refusal(
    repository_open: bool,
    opened_path: Option<&str>,
    at_revision: bool,
    untracked: bool,
    exact_path: bool,
) -> Option<String> {
    if !repository_open {
        return Some("no repository is open".to_owned());
    }
    let Some(path) = opened_path else {
        return Some("no file with a known path is open".to_owned());
    };
    if !at_revision {
        return Some("nothing is selected to blame at".to_owned());
    }
    if untracked {
        return Some(format!("{path} is untracked; it has no history to blame"));
    }
    if !exact_path {
        return Some(path_refusal("blame", path));
    }
    None
}

fn blame_spans(
    repository: &dyn RepositoryReading,
    target: &BlameTarget,
) -> ReadResult<Vec<BlameSpan>> {
    let since = blame_floor(repository.commit_details(target.tip)?.committer_time);
    let ranges = match repository.blame(target.tip, &target.path, target.limit, Some(since))? {
        Blame::Ranges(ranges) => ranges,
        Blame::TooLarge { size, limit } => {
            return Err(RepositoryError::new(format!(
                "cannot blame '{}': it is {size} bytes, over the {limit} byte limit",
                target.path
            )));
        }
    };
    let mut commits: HashMap<Oid, CommitSummary> = HashMap::new();
    let mut spans = Vec::with_capacity(ranges.len());
    for range in ranges {
        let commit = if let Some(cached) = commits.get(&range.commit) {
            cached.clone()
        } else {
            let read = repository.commit(range.commit)?;
            commits.insert(range.commit, read.clone());
            read
        };
        let older_than = range.boundary.then_some(since);
        spans.push(BlameSpan {
            range,
            commit,
            older_than,
        });
    }
    Ok(spans)
}

impl BrowserModel {
    /// Blames the file in the file pane at the browsed revision, pairing each range with its
    /// commit, each commit read once. The working tree has no revision of its own, so there
    /// the walk blames `HEAD`'s version, and the title says so; an untracked file is refused.
    /// The walk stops at [`blame_floor`] of the tip's committer time and is sized against the
    /// limit the file pane read with.
    ///
    /// A request for the file and revision a walk already runs for joins that walk by
    /// returning at once. A result is dropped when a newer request or
    /// [`Self::cancel_blame`] superseded it; when the open file or revision changed while it
    /// ran, the state goes back to empty instead.
    pub fn request_blame(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let target = self.blame_target();
        let refusal = blame_refusal(
            self.repository.is_some(),
            self.opened_file_path.as_deref(),
            target.is_some(),
            self.opened_file_is_untracked(),
            self.opened_path_is_exact,
        );
        if let Some(refusal) = refusal {
            self.publish_blame(BlameState::Failed(refusal), cx);
            return Task::ready(());
        }
        let (Some(repository), Some(target)) = (self.repository.clone(), target) else {
            return Task::ready(());
        };
        if self.blame_in_flight.as_ref() == Some(&target) {
            return Task::ready(());
        }
        self.generations.blame += 1;
        let generation = self.generations.blame;
        self.blame_in_flight = Some(target.clone());
        self.publish_blame(
            BlameState::Loading {
                path: target.path.clone(),
                title: blaming_title(&target.path),
            },
            cx,
        );
        let walked = target.clone();
        let work = blocking(cx.background_executor(), move || {
            blame_spans(&*repository, &walked)
        });
        Self::run(cx, (), async move |this, cx| {
            let result = work.await;
            with(&this, cx, |model, cx| {
                if generation != model.generations.blame {
                    return;
                }
                model.blame_in_flight = None;
                if model.blame_target().as_ref() != Some(&target) {
                    model.publish_blame(BlameState::Empty, cx);
                    return;
                }
                let state = match result {
                    Ok(spans) => BlameState::Loaded {
                        title: blame_title(&target.path, target.at_head),
                        path: target.path,
                        spans,
                    },
                    Err(error) => BlameState::Failed(error.message().to_owned()),
                };
                model.publish_blame(state, cx);
            });
        })
    }

    /// Drops the running walk's result, so it never reaches a closed window. A blame still
    /// loading becomes empty; a finished one stays.
    pub fn cancel_blame(&mut self, cx: &mut Context<Self>) {
        self.generations.blame += 1;
        self.blame_in_flight = None;
        if let BlameState::Loading { .. } = self.blame {
            self.publish_blame(BlameState::Empty, cx);
        }
    }

    /// Whether Show Blame can run: a file with a known path is open at a revision, which is
    /// `HEAD` in the working tree, and it is not untracked.
    #[must_use]
    pub fn can_blame_opened_file(&self) -> bool {
        self.blame_target().is_some() && !self.opened_file_is_untracked()
    }

    /// True when the open file was read from disk in the working tree and `HEAD` lacks it.
    fn opened_file_is_untracked(&self) -> bool {
        if !self.browsing_working_tree {
            return false;
        }
        let Some(OpenedFileSource::Disk { status, .. }) = &self.opened_file_source else {
            return false;
        };
        *status == Some(WorktreeStatus::Untracked)
    }

    /// What a blame asked for now would walk, or `None` when no file with a known path is open
    /// at a committed revision. The file generation makes a reopen count as another file.
    fn blame_target(&self) -> Option<BlameTarget> {
        let tip = if self.browsing_working_tree {
            self.head.as_ref().and_then(|head| head.target)
        } else {
            self.selected_ref
                .as_ref()
                .map(|reference| reference.target)
                .or_else(|| self.browsed_commit.as_ref().map(|commit| commit.oid))
        }?;
        Some(BlameTarget {
            path: self.opened_file_path.clone()?,
            tip,
            limit: self.opened_file_limit,
            file: self.generations.file,
            at_head: self.browsing_working_tree,
        })
    }

    fn publish_blame(&mut self, state: BlameState, cx: &mut Context<Self>) {
        self.blame = state;
        Self::changed(BrowserEvent::BlameChanged, cx);
    }
}
