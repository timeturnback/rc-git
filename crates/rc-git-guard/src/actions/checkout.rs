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

use super::commit::{LockedIndex, work_dir};
use super::{ActionError, ActionResult, confirm, git_err, open, treeapply};
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
/// [`ActionError::DirtyPath`](super::ActionError::DirtyPath) when a path the switch touches
/// has uncommitted work (nothing is written), [`ActionError::Git`](super::ActionError::Git)
/// when the branch does not exist, has no commit, the repository cannot be read or written,
/// a git operation (merge, rebase, cherry-pick, revert, bisect) is in progress, or the index
/// holds an unresolved conflict.
pub fn checkout_branch(
    repo_path: &Path,
    policy: &Policy,
    branch: &str,
) -> ActionResult<CheckoutOutcome> {
    let repo = open(repo_path, OP, branch)?;
    super::refuse_any_operation_in_progress(&repo, OP)?;
    let decision = policy.evaluate(&Action::Checkout {
        target: branch.to_owned(),
    });
    let _proof = confirm(decision, OP, branch, None)?;

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
    let current = super::head_commit(&repo, OP)?;
    let from = match (&head, current) {
        (Some(name), _) => name.shorten().to_string(),
        (None, Some(commit)) => commit.to_string(),
        (None, None) => return Err(neither_branch_nor_commit()),
    };

    let changes = treeapply::plan(repo_path, current, target, OP)?;
    let work_dir = work_dir(&repo, OP)?;
    let index = LockedIndex::acquire(&repo, OP)?;
    treeapply::verify_clean(&repo, &work_dir, &index, &changes, OP)?;
    let reflog = open_head_reflog(&repo)?;
    treeapply::apply(&repo, &work_dir, index, &changes, OP)?;
    point_head_at(&repo, &full_name, head.map(Target::Symbolic), current)?;
    if let Some(file) = reflog {
        log_head_switch(&repo, file, &from, branch, (current, target))?;
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
    repo: &gix::Repository,
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
    super::apply_edit(repo, edit, OP, "HEAD")
}

/// A `HEAD` that names no branch and resolves to no commit: nothing to plan the switch from.
fn neither_branch_nor_commit() -> ActionError {
    ActionError::git(OP, "HEAD", "names neither a branch nor a commit")
}

/// Opens `logs/HEAD` for append before anything is written, so a log the switch cannot
/// extend refuses the whole operation instead of failing after `HEAD` has moved. `None` when
/// the log does not exist: it is appended only when it already exists, which is what
/// `force_create_reflog: false` means on the edit itself.
fn open_head_reflog(repo: &gix::Repository) -> ActionResult<Option<std::fs::File>> {
    let path = repo.git_dir().join("logs").join("HEAD");
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    let opened = std::fs::OpenOptions::new().append(true).open(&path);
    match opened {
        Ok(file) => Ok(Some(file)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(git_err(OP, "logs/HEAD")(err)),
    }
}

/// Appends the `checkout: moving from …` line git writes to `logs/HEAD`. A ref transaction
/// will not write it: a symbolic update carries no object id, so gix skips the reflog for it
/// by design. The line is serialized by gix's own reflog writer and carries the same
/// synthetic `rc-git` committer as every other ref edit.
fn log_head_switch(
    repo: &gix::Repository,
    mut file: std::fs::File,
    from: &str,
    to: &str,
    (old, new): (Option<ObjectId>, ObjectId),
) -> ActionResult<()> {
    use std::io::Write;
    let line = gix::refs::log::Line {
        previous_oid: old.unwrap_or_else(|| ObjectId::null(repo.object_hash())),
        new_oid: new,
        signature: gix::actor::Signature {
            name: super::REFLOG_NAME.into(),
            email: super::REFLOG_EMAIL.into(),
            time: gix::date::Time::now_local_or_utc(),
        },
        message: format!("checkout: moving from {from} to {to}").into(),
    };
    let mut serialized = Vec::new();
    line.write_to(&mut serialized)
        .map_err(git_err(OP, "logs/HEAD"))?;
    file.write_all(&serialized)
        .map_err(git_err(OP, "logs/HEAD"))
}
