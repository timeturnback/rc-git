//! Discard uncommitted changes: restore tracked files to their `HEAD` bytes, delete
//! untracked files, or splice selected hunks out of one file. Destructive under the policy,
//! so every entry point goes through [`confirm`] before touching disk. The challenge carries
//! no typed phrase: a plain confirm answers it.

use std::path::Path;

use gix::objs::tree::EntryKind;

use super::commit::{
    IndexChange, LockedIndex, blob_data, disk_stat, tree_entry_at, update_index, verified_hunks,
    work_dir,
};
use super::workfile::{self, DiskEntry};
use super::{ActionError, ActionResult, HunkStamp, confirm, open, plan_discard};
use crate::Policy;

const OP: &str = "discard";

/// What discarding did to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardKind {
    /// The path is tracked in `HEAD`; its blob bytes and mode were written back.
    Restored,
    /// The path was untracked; the file was deleted (or was already gone).
    Deleted,
}

/// What [`discard_files`] will do to one path, decided before the first write.
enum Plan {
    /// Write this `HEAD` entry's bytes and mode back.
    Restore(gix::objs::tree::Entry),
    /// Delete the untracked file.
    Delete,
}

/// One path [`discard_files`] handled, in the order the caller listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardOutcome {
    /// The repository-relative path as the caller passed it.
    pub path: String,
    /// What happened to it.
    pub kind: DiscardKind,
}

/// Discards every change at `paths`: a path tracked in `HEAD` gets its blob bytes written
/// back to disk, an untracked path is deleted, and `.git/index` gets the `HEAD` entry for
/// each of them (or loses it), so a staged change is discarded too. Every path is checked
/// before the first write, so a refused path leaves every other one untouched. The policy asks for a plain confirm,
/// so `input` may be `None`. Returns one outcome per path, in order.
///
/// # Errors
/// [`ActionError::Git`] when a path is one git refuses (`.git`, `.`, `..`, an empty segment,
/// a symlinked parent), names a directory in `HEAD` or on disk, is untracked without a file
/// of exactly that name on disk, or can be neither restored nor deleted, and when another git
/// process holds `index.lock`. Only a failed write can stop the run part-way, after the paths
/// before it were handled; the index is then left as it was.
pub fn discard_files(
    repo_path: &Path,
    policy: &Policy,
    paths: &[String],
    input: Option<&str>,
) -> ActionResult<Vec<DiscardOutcome>> {
    let repo = open(repo_path, OP, "working tree")?;
    let _proof = confirm(plan_discard(policy, paths.len()), OP, "working tree", input)?;
    let work_dir = work_dir(&repo, OP)?;
    workfile::check_paths(&work_dir, paths.iter().map(String::as_str), OP)?;
    let head_tree = head_tree(&repo)?;
    let mut plan = Vec::with_capacity(paths.len());
    for path in paths {
        let entry = match tree_entry_at(&repo, OP, head_tree, path)? {
            Some(entry) if entry.mode.is_tree() => {
                return Err(ActionError::git(
                    OP,
                    path.clone(),
                    "is a directory; select files instead",
                ));
            }
            Some(entry) => Plan::Restore(entry),
            None => {
                workfile::require_exact_file_name(&work_dir, path, OP)?;
                Plan::Delete
            }
        };
        plan.push((path, entry));
    }
    let index = LockedIndex::acquire(&repo, OP)?;
    let mut outcomes = Vec::with_capacity(plan.len());
    let mut index_changes: Vec<IndexChange> = Vec::with_capacity(plan.len());
    for (path, entry) in plan {
        let (kind, change) = match entry {
            Plan::Restore(entry) => (
                DiscardKind::Restored,
                restore_entry(&repo, &work_dir, path, &entry, OP)?,
            ),
            Plan::Delete => {
                workfile::delete_file(OP, &work_dir, path)?;
                (DiscardKind::Deleted, (path.clone(), None))
            }
        };
        index_changes.push(change);
        outcomes.push(DiscardOutcome {
            path: path.clone(),
            kind,
        });
    }
    update_index(index, OP, &index_changes)?;
    Ok(outcomes)
}

/// Writes `entry`'s bytes and mode back to `path` and returns the index change that records
/// it: the blob id, kind, and the stat of the file just written.
fn restore_entry(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    entry: &gix::objs::tree::Entry,
    op: &'static str,
) -> ActionResult<IndexChange> {
    let kind = entry.mode.into();
    let data = blob_data(repo, op, path, entry.oid)?;
    workfile::restore_file(op, work_dir, path, &data, kind)?;
    let stat = disk_stat(work_dir, path, op);
    Ok((path.to_owned(), Some((entry.oid, kind, stat))))
}

/// Puts each of `paths` back to its state in `tree`, on disk and in `index`: a tracked file
/// gets its blob bytes and mode written back and its index entry set to that blob, an
/// untracked one is deleted and its index entry removed. Entries for other paths are kept.
/// Callers refuse directory entries first.
///
/// # Errors
/// [`ActionError::Git`] when an entry cannot be read, a file can be neither restored nor
/// deleted, or the index cannot be written.
pub(super) fn restore_paths(
    repo: &gix::Repository,
    work_dir: &Path,
    index: LockedIndex,
    tree: Option<gix::ObjectId>,
    paths: &[String],
    op: &'static str,
) -> ActionResult<()> {
    let mut index_changes: Vec<IndexChange> = Vec::with_capacity(paths.len());
    for path in paths {
        if let Some(entry) = tree_entry_at(repo, op, tree, path)? {
            index_changes.push(restore_entry(repo, work_dir, path, &entry, op)?);
        } else {
            workfile::delete_file(op, work_dir, path)?;
            index_changes.push((path.clone(), None));
        }
    }
    update_index(index, op, &index_changes)
}

/// Discards only `stamps` of `path`: the diff is recomputed, every stamp must match a
/// current hunk, and the file is rewritten with the unselected changes kept — that is, the
/// selected hunks' changes are gone. The policy asks for a plain confirm, so `input` may be
/// `None`.
///
/// # Errors
/// [`ActionError::HunkMismatch`] when the file moved under the selection,
/// [`ActionError::Git`] when `path` is one git refuses or the file cannot be read or written.
pub fn discard_hunks(
    repo_path: &Path,
    policy: &Policy,
    path: &str,
    stamps: &[HunkStamp],
    input: Option<&str>,
) -> ActionResult<()> {
    let repo = open(repo_path, OP, path)?;
    let _proof = confirm(plan_discard(policy, 1), OP, path, input)?;
    let mismatch = || ActionError::HunkMismatch {
        path: path.to_owned(),
    };
    if stamps.is_empty() {
        return Err(mismatch());
    }
    let work_dir = work_dir(&repo, OP)?;
    workfile::join_under(&work_dir, path, OP)?;
    let entry = tree_entry_at(&repo, OP, head_tree(&repo)?, path)?.ok_or_else(mismatch)?;
    if !entry.mode.is_blob() {
        return Err(mismatch());
    }
    let hunks = verified_hunks(repo_path, OP, path, entry.oid, stamps)?;
    // Keeping the unselected hunks and dropping the selected ones is one splice: apply
    // only the kept hunks onto the HEAD bytes.
    let kept: Vec<HunkStamp> = hunks
        .iter()
        .map(HunkStamp::from)
        .filter(|stamp| !stamps.contains(stamp))
        .collect();
    let old = blob_data(&repo, OP, path, entry.oid)?;
    // The file keeps its on-disk kind: hunk discard never touches the exec bit.
    let (new, kind) = match workfile::disk_entry(&work_dir, path, OP)? {
        DiskEntry::Present {
            data,
            kind: kind @ (EntryKind::Blob | EntryKind::BlobExecutable),
        } => (data, kind),
        DiskEntry::Present { .. } | DiskEntry::Directory => return Err(mismatch()),
        DiskEntry::Absent => return Err(ActionError::git(OP, path, "does not exist on disk")),
    };
    let result = workfile::splice(&old, &new, &kept).map_err(|_| mismatch())?;
    workfile::restore_file(OP, &work_dir, path, &result, kind)
}

/// The tree `HEAD` points at; `None` on an unborn branch, where everything is untracked.
fn head_tree(repo: &gix::Repository) -> ActionResult<Option<gix::ObjectId>> {
    let Some(head) = super::head_commit(repo, OP)? else {
        return Ok(None);
    };
    super::commit::tree_of(repo, OP, head).map(Some)
}
