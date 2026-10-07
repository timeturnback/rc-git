//! Fast-forward the checked-out branch to its remote-tracking ref. Local only — the
//! caller fetches first. The working tree and index are updated for exactly the
//! changed paths, and any dirty overlap refuses the whole operation before a single
//! byte is written. The branch ref moves last, so an interruption leaves visibly
//! modified files rather than what looks like lost work.

use std::path::Path;

use gix::ObjectId;
use gix::refs::Target;
use gix::refs::transaction::PreviousValue;

use crate::actions::commit::work_dir;
use crate::actions::{ActionError, ActionResult, confirm, open, treeapply};
use crate::{Action, Policy};

use super::{count_ahead_behind, current_branch, is_ancestor, peel_ref, tracking_ref_name};

const OP: &str = "pull";

/// What a fast-forward pull did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    /// Short name of the branch that moved.
    pub branch: String,
    /// Where the branch was before the pull.
    pub old: ObjectId,
    /// Where the branch is now. Equal to `old` when there was nothing to pull.
    pub new: ObjectId,
    /// Files the working tree changed.
    pub files_changed: usize,
}

/// Fast-forwards the checked-out branch to its remote-tracking ref, updating the
/// working tree, the index, and — last — the branch ref. No network I/O.
///
/// # Errors
/// [`ActionError::Diverged`] when both sides moved, [`ActionError::DirtyPath`] when a
/// changed path has uncommitted work (nothing is written), [`ActionError::NoUpstream`]
/// when the branch has no tracking ref, [`ActionError::DetachedHead`], and
/// [`ActionError::Git`] for everything else.
pub fn pull_fast_forward(repo_path: &Path, policy: &Policy) -> ActionResult<PullOutcome> {
    let repo = open(repo_path, OP, "repository")?;
    let (branch, tip) = current_branch(&repo, OP)?;
    let decision = policy.evaluate(&Action::PullFastForward {
        branch: branch.clone(),
    });
    let _proof = confirm(decision, OP, &branch, None)?;
    let old = tip.ok_or_else(|| {
        ActionError::git(
            OP,
            branch.clone(),
            "the branch has no commits yet; cannot pull into an unborn branch",
        )
    })?;
    let upstream_ref =
        tracking_ref_name(&repo, &branch).ok_or_else(|| ActionError::NoUpstream {
            op: OP,
            branch: branch.clone(),
        })?;
    let new = peel_ref(&repo, &upstream_ref).ok_or_else(|| ActionError::NoUpstream {
        op: OP,
        branch: branch.clone(),
    })?;

    if new == old || is_ancestor(&repo, new, old) {
        return Ok(PullOutcome {
            branch,
            old,
            new: old,
            files_changed: 0,
        });
    }
    if !is_ancestor(&repo, old, new) {
        let (ahead, behind) = count_ahead_behind(&repo, old, new, OP)?;
        return Err(ActionError::Diverged {
            branch,
            ahead,
            behind,
        });
    }

    let changes = treeapply::plan(repo_path, Some(old), new, OP)?;
    let work_dir = work_dir(&repo, OP)?;
    treeapply::verify_clean(&repo, &work_dir, &changes, OP)?;
    treeapply::apply(&repo, &work_dir, &changes, OP)?;
    advance_ref(&repo, &branch, old, new)?;
    Ok(PullOutcome {
        branch,
        old,
        new,
        files_changed: changes.len(),
    })
}

/// Moves the branch ref from `old` to `new` through the one ref-edit funnel, insisting
/// the branch has not moved since the pull was planned.
fn advance_ref(
    repo: &gix::Repository,
    branch: &str,
    old: ObjectId,
    new: ObjectId,
) -> ActionResult<()> {
    let full_name = format!("refs/heads/{branch}");
    let edit = crate::actions::update_edit(
        &full_name,
        PreviousValue::MustExistAndMatch(Target::Object(old)),
        Target::Object(new),
        "rc-git: pull --ff-only",
        OP,
    )?;
    crate::actions::apply_edit(repo, edit, OP, &full_name)
}
