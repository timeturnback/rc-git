//! Fast-forward the checked-out branch to its remote-tracking ref. Local only — the
//! caller fetches first. The working tree and index are updated for exactly the
//! changed paths, and any dirty overlap refuses the whole operation before a single
//! byte is written. The branch ref moves last, so an interruption leaves visibly
//! modified files rather than what looks like lost work.

use std::path::Path;

use gix::ObjectId;
use gix::objs::tree::EntryKind;

use crate::actions::commit::{blob_data, update_index, work_dir};
use crate::actions::{ActionError, ActionResult, confirm, open, workfile};
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

/// One path the fast-forward touches, with its blob on each side.
struct PathChange {
    path: String,
    old: Option<(ObjectId, EntryKind)>,
    new: Option<(ObjectId, EntryKind)>,
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
    let old = tip.ok_or_else(|| ActionError::Git {
        op: OP,
        name: branch.clone(),
        source: "the branch has no commits yet; cannot pull into an unborn branch".into(),
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

    let changes = plan_changes(repo_path, old, new)?;
    let work_dir = work_dir(&repo)?;
    verify_clean(&repo, &work_dir, &changes)?;

    let mut index_changes: Vec<(String, Option<(ObjectId, EntryKind)>)> = Vec::new();
    for change in &changes {
        if let Some((oid, kind)) = change.new {
            let data = blob_data(&repo, &change.path, oid)?;
            workfile::restore_file(&work_dir, &change.path, &data, kind)?;
            index_changes.push((change.path.clone(), Some((oid, kind))));
        } else {
            workfile::delete_file(&work_dir, &change.path)?;
            index_changes.push((change.path.clone(), None));
        }
    }
    update_index(&repo, &work_dir, &index_changes)?;
    advance_ref(&repo, &branch, old, new)?;
    Ok(PullOutcome {
        branch,
        old,
        new,
        files_changed: changes.len(),
    })
}

/// The per-path changes between the two commits, renames split into a deletion and an
/// addition. Submodules are refused.
fn plan_changes(repo_path: &Path, old: ObjectId, new: ObjectId) -> ActionResult<Vec<PathChange>> {
    let core = rc_git_core::Repository::open(repo_path).map_err(|source| ActionError::Git {
        op: OP,
        name: "repository".to_owned(),
        source: Box::new(source),
    })?;
    let from = rc_git_core::Oid::from_bytes(old.as_slice()).ok_or_else(|| ActionError::Git {
        op: OP,
        name: old.to_string(),
        source: "not a valid object id".into(),
    })?;
    let to = rc_git_core::Oid::from_bytes(new.as_slice()).ok_or_else(|| ActionError::Git {
        op: OP,
        name: new.to_string(),
        source: "not a valid object id".into(),
    })?;
    let tree_changes = core
        .diff_trees(Some(from), to)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: new.to_string(),
            source: Box::new(source),
        })?;

    let mut changes = Vec::new();
    for change in tree_changes {
        let path = change.path.to_string();
        let kind = entry_kind(&path, change.entry)?;
        let old_blob = change
            .old
            .and_then(|oid| ObjectId::try_from(oid.as_bytes()).ok());
        let new_blob = change
            .new
            .and_then(|oid| ObjectId::try_from(oid.as_bytes()).ok());
        match change.kind {
            rc_git_core::ChangeKind::Added => changes.push(PathChange {
                path,
                old: None,
                new: new_blob.map(|id| (id, kind)),
            }),
            rc_git_core::ChangeKind::Deleted => changes.push(PathChange {
                path,
                old: old_blob.map(|id| (id, kind)),
                new: None,
            }),
            rc_git_core::ChangeKind::Modified => changes.push(PathChange {
                path,
                old: old_blob.map(|id| (id, kind)),
                new: new_blob.map(|id| (id, kind)),
            }),
            rc_git_core::ChangeKind::Renamed => {
                if let Some(source_path) = &change.source_path {
                    changes.push(PathChange {
                        path: source_path.to_string(),
                        old: old_blob.map(|id| (id, kind)),
                        new: None,
                    });
                }
                changes.push(PathChange {
                    path,
                    old: None,
                    new: new_blob.map(|id| (id, kind)),
                });
            }
            rc_git_core::ChangeKind::Copied => changes.push(PathChange {
                path,
                old: None,
                new: new_blob.map(|id| (id, kind)),
            }),
        }
    }
    Ok(changes)
}

/// Maps the core's entry kind back to gix's, refusing what a pull cannot write.
fn entry_kind(path: &str, kind: rc_git_core::EntryKind) -> ActionResult<EntryKind> {
    match kind {
        rc_git_core::EntryKind::Blob => Ok(EntryKind::Blob),
        rc_git_core::EntryKind::ExecutableBlob => Ok(EntryKind::BlobExecutable),
        rc_git_core::EntryKind::Symlink => Ok(EntryKind::Link),
        rc_git_core::EntryKind::Submodule => Err(ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: "pulling changes to a submodule is not supported; use git directly".into(),
        }),
        rc_git_core::EntryKind::Tree => Err(ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: "unexpected directory entry in a tree diff".into(),
        }),
    }
}

/// Refuses the pull when any touched path is dirty on disk or staged in the index,
/// before anything is written.
fn verify_clean(
    repo: &gix::Repository,
    work_dir: &Path,
    changes: &[PathChange],
) -> ActionResult<()> {
    let index = gix::index::File::at_or_default(
        repo.index_path(),
        repo.object_hash(),
        false,
        gix::index::decode::Options::default(),
    )
    .map_err(|source| ActionError::Git {
        op: OP,
        name: "index".to_owned(),
        source: Box::new(source),
    })?;
    for change in changes {
        let dirty = || ActionError::DirtyPath {
            path: change.path.clone(),
        };
        if let Some((old_blob, kind)) = change.old {
            let expected = blob_data(repo, &change.path, old_blob)?;
            if !disk_matches(work_dir, &change.path, &expected, kind)? {
                return Err(dirty());
            }
        } else {
            // The upstream adds this path; anything already there is in the way.
            let full = workfile::join_under(work_dir, &change.path)?;
            if std::fs::symlink_metadata(&full).is_ok() {
                return Err(dirty());
            }
        }
        let staged = index
            .entry_by_path(change.path.as_bytes().into())
            .map(|entry| entry.id);
        let expected = change.old.map(|(oid, _)| oid);
        if staged != expected {
            return Err(dirty());
        }
    }
    Ok(())
}

/// Whether the file on disk holds exactly `data`, honoring the entry kind.
fn disk_matches(work_dir: &Path, path: &str, data: &[u8], kind: EntryKind) -> ActionResult<bool> {
    let full = workfile::join_under(work_dir, path)?;
    match kind {
        EntryKind::Link => {
            use std::os::unix::ffi::OsStrExt;
            match std::fs::read_link(&full) {
                Ok(target) => Ok(target.as_os_str().as_bytes() == data),
                Err(_) => Ok(false),
            }
        }
        EntryKind::Blob | EntryKind::BlobExecutable => match std::fs::read(&full) {
            Ok(disk) => Ok(disk == data),
            Err(_) => Ok(false),
        },
        EntryKind::Tree | EntryKind::Commit => Ok(false),
    }
}

/// Moves the branch ref from `old` to `new` through the one ref-edit funnel, insisting
/// the branch has not moved since the pull was planned.
fn advance_ref(
    repo: &gix::Repository,
    branch: &str,
    old: ObjectId,
    new: ObjectId,
) -> ActionResult<()> {
    use gix::refs::Target;
    use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
    let full_name = format!("refs/heads/{branch}");
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "rc-git: pull --ff-only".into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(old)),
            new: Target::Object(new),
        },
        name: full_name.as_str().try_into().map_err(
            |source: gix::validate::reference::name::Error| ActionError::Git {
                op: OP,
                name: full_name.clone(),
                source: Box::new(source),
            },
        )?,
        deref: false,
    };
    crate::actions::apply_edit(repo, edit, OP, &full_name)
}
