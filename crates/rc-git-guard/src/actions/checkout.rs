//! Switching `HEAD` to another local branch. The working tree and index are updated for
//! exactly the paths that differ between the two commits, and any dirty overlap refuses the
//! whole operation before a single byte is written — so a file with uncommitted work is never
//! overwritten, and files the switch does not touch carry over as git leaves them.
//!
//! `HEAD` moves last. An interruption therefore leaves visibly modified files on the branch
//! the user started from, rather than what looks like lost work.

use std::path::Path;

use gix::ObjectId;
use gix::refs::Target;
use gix::refs::transaction::PreviousValue;

use super::commit::work_dir;
use super::write::Writer;
use super::{ActionError, ActionResult, authorize, git_err, open, treeapply};
use crate::{Action, Policy};

const OP: &str = "checkout";

/// What a branch switch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutOutcome {
    /// Short name of the branch now checked out.
    pub branch: String,
    /// The commit `HEAD` resolves to afterwards.
    pub new_head: rc_git_core::Oid,
    /// Files the working tree changed.
    pub files_changed: usize,
    /// Whether `HEAD` was already on this branch, so nothing was written.
    pub already_on_branch: bool,
}

/// Checks out the local branch `branch`, updating the working tree, the index, and — last —
/// `HEAD`. Creates nothing: the branch must already exist locally.
///
/// # Errors
/// [`ActionError::DirtyPath`] when a path the switch touches has uncommitted work (nothing is
/// written), [`ActionError::Git`] when the branch does not exist, has no commit, is checked out in another worktree, the
/// repository cannot be read or written,
/// a git operation (merge, rebase, cherry-pick, revert, bisect) is in progress, or the index
/// holds an unresolved conflict.
pub fn checkout_branch(
    repo_path: &Path,
    policy: &Policy,
    branch: &str,
) -> ActionResult<CheckoutOutcome> {
    let repo = open(repo_path, OP, branch)?;
    super::refuse_any_operation_in_progress(&repo, OP)?;
    let action = Action::Checkout {
        target: branch.to_owned(),
    };
    let proof = authorize(policy, &action, OP, branch, None)?;
    let writer = Writer::new(&repo, &proof);

    let full_name = format!("refs/heads/{branch}");
    let target = branch_tip(&repo, &full_name)?;
    let new_head = treeapply::core_oid(target, OP)?;
    let head = repo.head_name().map_err(git_err(OP, "HEAD"))?;
    if head
        .as_ref()
        .is_some_and(|name| name.as_bstr() == full_name.as_str())
    {
        return Ok(CheckoutOutcome {
            branch: branch.to_owned(),
            new_head,
            files_changed: 0,
            already_on_branch: true,
        });
    }
    if let Some(path) = super::branch_checked_out_at(&repo, OP, &full_name)? {
        return Err(ActionError::git(
            OP,
            branch,
            format!("already used by worktree at '{path}'"),
        ));
    }
    let current = super::head_commit(&repo, OP)?;
    let from = match (&head, current) {
        (Some(name), _) => name.shorten().to_string(),
        (None, Some(commit)) => commit.to_string(),
        (None, None) => return Err(neither_branch_nor_commit()),
    };

    let changes = treeapply::plan(repo_path, current, target, OP)?;
    let work_dir = work_dir(&repo, OP)?;
    let index = writer.lock_index(OP)?;
    treeapply::verify_clean(&repo, &work_dir, &index, &changes, OP)?;
    let reflog = writer.open_head_reflog(OP)?;
    treeapply::apply(&writer, &work_dir, index, &changes, OP)?;
    point_head_at(&writer, &full_name, head.map(Target::Symbolic), current)?;
    if let Some(file) = reflog {
        let message = format!("checkout: moving from {from} to {branch}");
        writer.append_head_reflog(file, (current, target), &message, OP)?;
    }
    Ok(CheckoutOutcome {
        branch: branch.to_owned(),
        new_head,
        files_changed: changes.len(),
        already_on_branch: false,
    })
}

/// The commit a local branch points at.
fn branch_tip(repo: &gix::Repository, full_name: &str) -> ActionResult<ObjectId> {
    super::require_listed_ref(repo, OP, full_name)?;
    let reference = repo
        .find_reference(full_name)
        .map_err(git_err(OP, full_name))?;
    Ok(reference
        .into_fully_peeled_id()
        .map_err(git_err(OP, full_name))?
        .detach())
}

/// Rewrites `HEAD` to point at `full_name`, insisting it still holds what the switch was
/// planned against: the branch it was on, or the commit it was detached at. An unborn `HEAD`
/// that names a branch is covered by the symbolic expectation; with neither there is nothing
/// to compare against, so the switch is refused.
fn point_head_at(
    writer: &Writer<'_>,
    full_name: &str,
    previous_branch: Option<Target>,
    previous_commit: Option<ObjectId>,
) -> ActionResult<()> {
    let expected = match (previous_branch, previous_commit) {
        (Some(branch), _) => PreviousValue::MustExistAndMatch(branch),
        (None, Some(commit)) => PreviousValue::MustExistAndMatch(Target::Object(commit)),
        (None, None) => return Err(neither_branch_nor_commit()),
    };
    let edit = super::update_edit(
        "HEAD",
        expected,
        Target::Symbolic(full_name.try_into().map_err(git_err(OP, full_name))?),
        &format!("rc-git: checkout: moving to {full_name}"),
        OP,
    )?;
    writer.apply_edit(edit, OP, "HEAD")
}

/// A `HEAD` that names no branch and resolves to no commit: nothing to plan the switch from.
fn neither_branch_nor_commit() -> ActionError {
    ActionError::git(OP, "HEAD", "names neither a branch nor a commit")
}
