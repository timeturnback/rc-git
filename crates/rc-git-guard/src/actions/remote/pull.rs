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
use crate::actions::write::Writer;
use crate::actions::{ActionError, ActionResult, authorize, open, treeapply};
use crate::{Action, Policy};

use super::{count_ahead_behind, current_branch, is_ancestor, peel_ref, tracking_ref_name};

const OP: &str = "pull";
/// What the branch and `HEAD` reflogs record for a fast-forward pull.
const REFLOG_MESSAGE: &str = "rc-git: pull --ff-only";

/// What a fast-forward pull did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullOutcome {
    /// Short name of the branch that moved.
    pub branch: String,
    /// Where the branch was before the pull.
    pub old: rc_git_core::Oid,
    /// Where the branch is now. Equal to `old` when there was nothing to pull.
    pub new: rc_git_core::Oid,
    /// Files the working tree changed.
    pub files_changed: usize,
    /// Whether there was nothing to pull, so nothing was written.
    pub already_up_to_date: bool,
}

/// Fast-forwards the checked-out branch to its remote-tracking ref, updating the
/// working tree, the index, and — last — the branch ref. No network I/O.
///
/// # Errors
/// [`ActionError::Diverged`] when both sides moved, [`ActionError::DirtyPath`] when a
/// changed path has uncommitted work (nothing is written), [`ActionError::NoUpstream`]
/// when the branch has no tracking ref, [`ActionError::DetachedHead`], and
/// [`ActionError::Git`] for everything else, including a git operation in progress (merge,
/// rebase, cherry-pick, revert, bisect) and an unresolved conflict in the index.
pub fn pull_fast_forward(repo_path: &Path, policy: &Policy) -> ActionResult<PullOutcome> {
    let repo = open(repo_path, OP, "repository")?;
    crate::actions::refuse_any_operation_in_progress(&repo, OP)?;
    let (branch, tip) = current_branch(&repo, OP)?;
    let action = Action::PullFastForward {
        branch: branch.clone(),
    };
    let proof = authorize(policy, &action, OP, &branch, None)?;
    let writer = Writer::new(&repo, &proof);
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

    let (old_id, new_id) = (treeapply::core_oid(old, OP)?, treeapply::core_oid(new, OP)?);
    if new == old || is_ancestor(&repo, new, old) {
        return Ok(PullOutcome {
            branch,
            old: old_id,
            new: old_id,
            files_changed: 0,
            already_up_to_date: true,
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
    let index = writer.lock_index(OP)?;
    treeapply::verify_clean(&repo, &work_dir, &index, &changes, OP)?;
    let reflog = writer.open_head_reflog(OP)?;
    treeapply::apply(&writer, &work_dir, index, &changes, OP)?;
    advance_ref(&writer, &branch, old, new)?;
    if let Some(file) = reflog {
        writer.append_head_reflog(file, (Some(old), new), REFLOG_MESSAGE, OP)?;
    }
    Ok(PullOutcome {
        branch,
        old: old_id,
        new: new_id,
        files_changed: changes.len(),
        already_up_to_date: false,
    })
}

/// Moves the branch ref from `old` to `new` through the one ref-edit funnel, insisting
/// the branch has not moved since the pull was planned.
fn advance_ref(
    writer: &Writer<'_>,
    branch: &str,
    old: ObjectId,
    new: ObjectId,
) -> ActionResult<()> {
    let full_name = format!("refs/heads/{branch}");
    let edit = crate::actions::update_edit(
        &full_name,
        PreviousValue::MustExistAndMatch(Target::Object(old)),
        Target::Object(new),
        REFLOG_MESSAGE,
        OP,
    )?;
    writer.apply_edit(edit, OP, &full_name)
}
