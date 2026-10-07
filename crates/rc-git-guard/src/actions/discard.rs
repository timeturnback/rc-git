//! Discard uncommitted changes: restore tracked files to their `HEAD` bytes, delete
//! untracked files, or splice selected hunks out of one file. Destructive under the policy,
//! so every entry point goes through [`confirm`] before touching disk. The challenge carries
//! no typed phrase: a plain confirm answers it.

use std::path::Path;

use super::commit::{blob_data, recompute_hunks, stamp_matches, tree_entry_at, work_dir};
use super::workfile;
use super::{ActionError, ActionResult, HunkStamp, confirm, open};
use crate::{Action, Policy};

const OP: &str = "discard";

/// Discards every change at `paths`: a path tracked in `HEAD` gets its blob bytes written
/// back to disk, an untracked path is deleted. Every path is checked before the first write.
/// The policy asks for a plain confirm, so `input` may be `None`.
///
/// # Errors
/// [`ActionError::Git`] when a path is one git refuses (`.git`, `.`, `..`, an empty segment,
/// a symlinked parent), names a directory, is untracked without a file of exactly that name
/// on disk, or can be neither restored nor deleted.
pub fn discard_files(
    repo_path: &Path,
    policy: &Policy,
    paths: &[String],
    input: Option<&str>,
) -> ActionResult<()> {
    let repo = open(repo_path, OP, "working tree")?;
    let decision = policy.evaluate(&Action::DiscardWorkingChanges { files: paths.len() });
    let _proof = confirm(decision, OP, "working tree", input)?;
    let work_dir = work_dir(&repo, OP)?;
    workfile::check_paths(&work_dir, paths.iter().map(String::as_str), OP)?;
    let head_tree = head_tree(&repo)?;
    let mut planned = Vec::with_capacity(paths.len());
    for path in paths {
        let entry = tree_entry_at(&repo, OP, head_tree, path)?;
        match &entry {
            Some(entry) if entry.mode.is_tree() => {
                return Err(ActionError::Git {
                    op: OP,
                    name: path.clone(),
                    source: "is a directory; select files instead".into(),
                });
            }
            Some(_) => {}
            None => workfile::require_exact_name(&work_dir, path, OP)?,
        }
        planned.push((path, entry));
    }
    for (path, entry) in planned {
        match entry {
            Some(entry) => {
                let data = blob_data(&repo, OP, path, entry.oid)?;
                workfile::restore_file(OP, &work_dir, path, &data, entry.mode.into())?;
            }
            None => workfile::delete_file(OP, &work_dir, path)?,
        }
    }
    Ok(())
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
    let decision = policy.evaluate(&Action::DiscardWorkingChanges { files: 1 });
    let _proof = confirm(decision, OP, path, input)?;
    let mismatch = || ActionError::HunkMismatch {
        path: path.to_owned(),
    };
    if stamps.is_empty() {
        return Err(mismatch());
    }
    let work_dir = work_dir(&repo, OP)?;
    let full = workfile::join_under(&work_dir, path, OP)?;
    let entry = tree_entry_at(&repo, OP, head_tree(&repo)?, path)?.ok_or_else(mismatch)?;
    if !entry.mode.is_blob() {
        return Err(mismatch());
    }
    let hunks = recompute_hunks(repo_path, OP, path, entry.oid)?;
    for stamp in stamps {
        if !stamp_matches(*stamp, &hunks) {
            return Err(mismatch());
        }
    }
    // Keeping the unselected hunks and dropping the selected ones is one splice: apply
    // only the kept hunks onto the HEAD bytes.
    let kept: Vec<HunkStamp> = hunks
        .iter()
        .map(|hunk| HunkStamp {
            old_start: hunk.old_start,
            old_lines: hunk.old_lines,
            new_start: hunk.new_start,
            new_lines: hunk.new_lines,
        })
        .filter(|stamp| !stamps.contains(stamp))
        .collect();
    let old = blob_data(&repo, OP, path, entry.oid)?;
    let new = std::fs::read(&full).map_err(|source| ActionError::Git {
        op: OP,
        name: path.to_owned(),
        source: Box::new(source),
    })?;
    let result = workfile::splice(&old, &new, &kept).map_err(|_| mismatch())?;
    // The file keeps its on-disk kind: hunk discard never touches the exec bit.
    let kind = if is_executable(&full) {
        gix::objs::tree::EntryKind::BlobExecutable
    } else {
        gix::objs::tree::EntryKind::Blob
    };
    workfile::restore_file(OP, &work_dir, path, &result, kind)
}

/// The tree `HEAD` points at; `None` on an unborn branch, where everything is untracked.
fn head_tree(repo: &gix::Repository) -> ActionResult<Option<gix::ObjectId>> {
    let Some(head) = repo.head_id().ok().map(gix::Id::detach) else {
        return Ok(None);
    };
    super::commit::tree_of(repo, OP, head).map(Some)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}
