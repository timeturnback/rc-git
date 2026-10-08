//! Applying the difference between two commits to the working tree and the index: what a
//! fast-forward pull and a branch switch both do. Exactly the changed paths are touched, and
//! any dirty overlap refuses the whole operation before a single byte is written, so the
//! caller can move a ref afterwards knowing the tree already matches it.
//!
//! The caller writes its ref last. An interruption therefore leaves visibly modified files
//! rather than what looks like lost work.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use gix::ObjectId;
use gix::objs::tree::EntryKind;

use super::commit::{
    IndexChange, LockedIndex, blob_data, disk_stat, index_mode, tree_entry_at, tree_of,
    update_index,
};
use super::workfile::DiskEntry;
use super::{ActionError, ActionResult, git_err, workfile};

/// One path the change touches, with its blob on each side.
pub(super) struct PathChange {
    pub(super) path: String,
    pub(super) old: Option<(ObjectId, EntryKind)>,
    pub(super) new: Option<(ObjectId, EntryKind)>,
}

/// The per-path changes between two commits, renames split into a deletion and an addition.
/// `from` is `None` when there is no commit on the other side, as on an unborn branch.
/// Submodules are refused.
///
/// # Errors
/// [`ActionError::Git`] when the repository or either tree cannot be read, and for a path
/// this cannot write: a submodule, an unexpected directory entry, or a path git refuses to
/// check out (see [`workfile::validate_path`]). Every path is checked here, so a bad one
/// refuses the whole operation before anything is written.
pub(super) fn plan(
    repo_path: &Path,
    from: Option<ObjectId>,
    to: ObjectId,
    op: &'static str,
) -> ActionResult<Vec<PathChange>> {
    let core = rc_git_core::Repository::open(repo_path).map_err(git_err(op, "repository"))?;
    let from_core = match from {
        Some(from) => Some(core_oid(from, op)?),
        None => None,
    };
    let to_oid = core_oid(to, op)?;
    let tree_changes = core
        .diff_trees_without_renames(from_core, to_oid)
        .map_err(|source| ActionError::git(op, to.to_string(), source))?;

    let mut changes = Vec::new();
    for change in tree_changes {
        let path = change.path.to_string();
        let kind = entry_kind(&path, change.entry, op)?;
        workfile::validate_path(op, &path, kind == EntryKind::Link)?;
        if let Some(source_path) = &change.source_path {
            workfile::validate_path(op, &source_path.to_string(), kind == EntryKind::Link)?;
        }
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
        }
    }
    set_old_kinds(repo_path, from, &mut changes, op)?;
    Ok(changes)
}

/// Replaces each old side's kind with the one `from`'s tree records. The tree diff reports
/// one kind per change, the new side's, so a mode change such as `chmod +x` would otherwise
/// compare the disk against the new mode.
///
/// # Errors
/// [`ActionError::Git`] when the repository or the tree cannot be read, or an old path is
/// missing from it or is not a file or symlink there.
fn set_old_kinds(
    repo_path: &Path,
    from: Option<ObjectId>,
    changes: &mut [PathChange],
    op: &'static str,
) -> ActionResult<()> {
    if changes.iter().all(|change| change.old.is_none()) {
        return Ok(());
    }
    let repo = super::open(repo_path, op, "repository")?;
    let from_tree = match from {
        Some(commit) => Some(tree_of(&repo, op, commit)?),
        None => None,
    };
    for change in changes.iter_mut() {
        let Some((oid, _)) = change.old else {
            continue;
        };
        let entry = tree_entry_at(&repo, op, from_tree, &change.path)?.ok_or_else(|| {
            ActionError::git(op, change.path.as_str(), "missing from the old tree")
        })?;
        let kind = match entry.mode.kind() {
            kind @ (EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link) => kind,
            EntryKind::Tree | EntryKind::Commit => {
                return Err(ActionError::git(
                    op,
                    change.path.as_str(),
                    "is not a file or symlink in the old tree",
                ));
            }
        };
        change.old = Some((oid, kind));
    }
    Ok(())
}

/// Refuses the whole operation when any touched path is dirty on disk or staged in `index`,
/// before anything is written. Take `index` with [`LockedIndex::acquire`] before this call and
/// hand the same one to [`apply`], so no other process can change the index in between.
///
/// # Errors
/// [`ActionError::DirtyPath`] for the first path that does not match `HEAD` on disk or in the
/// index (blob id and mode), or for a file or
/// symlink standing where an added path needs a parent directory;
/// [`ActionError::Git`] when a blob cannot be read, or a path would cross a symlink (one on
/// disk, or one this plan writes).
pub(super) fn verify_clean(
    repo: &gix::Repository,
    work_dir: &Path,
    index: &LockedIndex,
    changes: &[PathChange],
    op: &'static str,
) -> ActionResult<()> {
    let index = index.file();
    refuse_unmerged_entries(index, op)?;
    refuse_paths_under_written_links(changes, op)?;
    let file_mode = repo
        .config_snapshot()
        .boolean("core.fileMode")
        .unwrap_or(true);
    let deleted_files = deleted_files_on_disk(work_dir, changes, op)?;
    let deleted_paths: HashSet<&str> = changes
        .iter()
        .filter(|change| change.new.is_none())
        .map(|change| change.path.as_str())
        .collect();
    for change in changes {
        let dirty = || ActionError::DirtyPath {
            op,
            path: change.path.clone(),
        };
        if let Some((old_blob, kind)) = change.old {
            let expected = blob_data(repo, op, &change.path, old_blob)?;
            if !disk_matches(work_dir, &change.path, (&expected, kind), file_mode, op)? {
                return Err(dirty());
            }
        } else {
            // The other side adds this path; anything already there is in the way, except a
            // file the plan deletes first or a directory the deletions empty.
            refuse_blocked_ancestors(work_dir, &change.path, &deleted_files, op)?;
            let full = workfile::join_under(work_dir, &change.path, op)?;
            if let Ok(meta) = std::fs::symlink_metadata(&full)
                && !deleted_files.contains(&file_id(&meta))
                && !(meta.is_dir() && emptied_by_plan(&full, &change.path, &deleted_paths, op)?)
            {
                return Err(dirty());
            }
        }
        let staged = index
            .entry_by_path(change.path.as_bytes().into())
            .map(|entry| (entry.id, entry.mode));
        let expected = change.old.map(|(oid, kind)| (oid, index_mode(kind)));
        if staged != expected {
            return Err(dirty());
        }
    }
    Ok(())
}

/// Refuses the operation when `index` holds a conflicted path (an entry at a non-zero stage), as
/// git does: a lookup by path returns only one stage, so the per-path checks would miss it.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and the first conflicted path.
fn refuse_unmerged_entries(index: &gix::index::File, op: &'static str) -> ActionResult<()> {
    let Some(entry) = index.entries().iter().find(|entry| entry.stage_raw() != 0) else {
        return Ok(());
    };
    Err(ActionError::git(
        op,
        entry.path(index).to_string(),
        "has unresolved conflicts; resolve them in git first",
    ))
}

/// Refuses an added `path` when one of its parent directories is something else on disk: an
/// untracked file or symlink standing where the plan needs a directory. A lstat of the leaf
/// alone reads that case as absent, and the write would then fail halfway through the plan.
/// An ancestor the plan deletes first is not in the way; nothing below it exists yet.
///
/// # Errors
/// [`ActionError::DirtyPath`] naming a blocking file, [`ActionError::Git`] for a blocking
/// symlink (the write would cross it) or an ancestor that cannot be inspected.
fn refuse_blocked_ancestors(
    work_dir: &Path,
    path: &str,
    deleted_files: &HashSet<(u64, u64)>,
    op: &'static str,
) -> ActionResult<()> {
    for (end, _) in path.match_indices('/') {
        let ancestor = &path[..end];
        let meta = match std::fs::symlink_metadata(work_dir.join(ancestor)) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(ActionError::git(op, ancestor, error)),
        };
        if meta.is_dir() {
            continue;
        }
        if deleted_files.contains(&file_id(&meta)) {
            return Ok(());
        }
        if meta.file_type().is_symlink() {
            return Err(workfile::crosses_link(op, path, ancestor));
        }
        return Err(ActionError::DirtyPath {
            op,
            path: ancestor.to_owned(),
        });
    }
    Ok(())
}

/// The device and inode of a file on disk: two spellings that a case-insensitive or
/// normalization-insensitive volume resolves to one file share it.
fn file_id(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

/// The files on disk the plan deletes. An added path that resolves to one of them exists only
/// because the volume folds its spelling onto a path the plan removes first, as in the
/// case-only rename `Readme.md` -> `README.md`; [`verify_clean`] checks that path is clean
/// like every other.
///
/// # Errors
/// [`ActionError::Git`] when a deleted path is invalid or crosses a symlink.
fn deleted_files_on_disk(
    work_dir: &Path,
    changes: &[PathChange],
    op: &'static str,
) -> ActionResult<HashSet<(u64, u64)>> {
    let mut files = HashSet::new();
    for change in changes.iter().filter(|change| change.new.is_none()) {
        let full = workfile::join_under(work_dir, &change.path, op)?;
        if let Ok(meta) = std::fs::symlink_metadata(&full) {
            files.insert(file_id(&meta));
        }
    }
    Ok(files)
}

/// Whether the directory `dir` (the repository path `relative`) is gone once [`apply`] has run
/// the plan's deletions: everything below it is a file in `deleted`, and every directory
/// below it holds at least one, so pruning after the last deletion removes it. Any other
/// entry, untracked or ignored, keeps it, and git refuses that switch too. An empty `dir`
/// counts as emptied, since [`workfile::restore_file`] removes an empty directory itself.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and the directory when it cannot be read.
fn emptied_by_plan(
    dir: &Path,
    relative: &str,
    deleted: &HashSet<&str>,
    op: &'static str,
) -> ActionResult<bool> {
    let entries = std::fs::read_dir(dir).map_err(git_err(op, relative))?;
    for entry in entries {
        let entry = entry.map_err(git_err(op, relative))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return Ok(false);
        };
        let path = format!("{relative}/{name}");
        let is_dir = entry.file_type().map_err(git_err(op, &path))?.is_dir();
        let gone = if is_dir {
            directory_holds_entries(&entry.path(), &path, op)?
                && emptied_by_plan(&entry.path(), &path, deleted, op)?
        } else {
            deleted.contains(path.as_str())
        };
        if !gone {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `dir` holds any entry. An empty subdirectory is never pruned, so it keeps its
/// parent in place.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and `relative` when the directory cannot be read.
fn directory_holds_entries(dir: &Path, relative: &str, op: &'static str) -> ActionResult<bool> {
    let mut entries = std::fs::read_dir(dir).map_err(git_err(op, relative))?;
    Ok(entries.next().is_some())
}

/// Refuses a plan that writes a symlink and also writes a path below a directory spelled like
/// it in another case. On a case-insensitive volume `Z` and `z` are one name, so writing
/// `z/p` after the link `Z` would follow it out of the working tree. The check is on the plan
/// because the link does not exist yet when [`verify_clean`] inspects the disk.
///
/// # Errors
/// [`ActionError::Git`] naming the path that would cross the link.
fn refuse_paths_under_written_links(changes: &[PathChange], op: &'static str) -> ActionResult<()> {
    let links: HashMap<String, &str> = changes
        .iter()
        .filter(|change| matches!(change.new, Some((_, EntryKind::Link))))
        .map(|change| (change.path.to_lowercase(), change.path.as_str()))
        .collect();
    if links.is_empty() {
        return Ok(());
    }
    for change in changes.iter().filter(|change| change.new.is_some()) {
        let folded = change.path.to_lowercase();
        for (end, _) in folded.match_indices('/') {
            if let Some(link) = links.get(&folded[..end]) {
                return Err(workfile::crosses_link(op, &change.path, link));
            }
        }
    }
    Ok(())
}

/// Writes every change to the working tree, then updates `index` for exactly those paths and
/// releases its lock. Deletions run before writes: on a case-insensitive volume a deleted
/// `Readme.md` is the same file as an added `README.md`, and deleting it afterwards would
/// remove the new file. Call [`verify_clean`] first, with the same `index`.
///
/// # Errors
/// [`ActionError::Git`] naming `op` when a file or the index cannot be written. A file that
/// fails part way through the plan names how many paths were already written, and that
/// neither `HEAD` nor the index was updated.
pub(super) fn apply(
    repo: &gix::Repository,
    work_dir: &Path,
    index: LockedIndex,
    changes: &[PathChange],
    op: &'static str,
) -> ActionResult<()> {
    let total = changes.len();
    let stopped = |error, written| {
        workfile::stopped_part_way(error, written, total, "HEAD and the index were not updated")
    };
    let mut index_changes: Vec<IndexChange> = Vec::with_capacity(total);
    for change in changes.iter().filter(|change| change.new.is_none()) {
        workfile::delete_file(op, work_dir, &change.path)
            .map_err(|error| stopped(error, index_changes.len()))?;
        index_changes.push((change.path.clone(), None));
    }
    for change in changes {
        if let Some((oid, kind)) = change.new {
            let written = write_blob(repo, work_dir, &change.path, (oid, kind), op)
                .map_err(|error| stopped(error, index_changes.len()))?;
            index_changes.push(written);
        }
    }
    update_index(index, op, &index_changes)
}

/// Writes blob `oid` to `path` as `kind` and returns the index change that records it.
fn write_blob(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    (oid, kind): (ObjectId, EntryKind),
    op: &'static str,
) -> ActionResult<IndexChange> {
    let data = blob_data(repo, op, path, oid)?;
    workfile::restore_file(op, work_dir, path, &data, kind)?;
    let stat = disk_stat(work_dir, path, op);
    Ok((path.to_owned(), Some((oid, kind, stat))))
}

pub(super) fn core_oid(oid: ObjectId, op: &'static str) -> ActionResult<rc_git_core::Oid> {
    rc_git_core::Oid::from_bytes(oid.as_slice())
        .ok_or_else(|| ActionError::git(op, oid.to_string(), "not a valid object id"))
}

/// Maps the core's entry kind back to gix's, refusing what these writes cannot produce.
///
/// # Errors
/// [`ActionError::Git`] naming `path` for a submodule or a directory entry.
pub(super) fn entry_kind(
    path: &str,
    kind: rc_git_core::EntryKind,
    op: &'static str,
) -> ActionResult<EntryKind> {
    match kind {
        rc_git_core::EntryKind::Blob => Ok(EntryKind::Blob),
        rc_git_core::EntryKind::ExecutableBlob => Ok(EntryKind::BlobExecutable),
        rc_git_core::EntryKind::Symlink => Ok(EntryKind::Link),
        rc_git_core::EntryKind::Submodule => Err(ActionError::git(
            op,
            path,
            "changing a submodule is not supported; use git directly",
        )),
        rc_git_core::EntryKind::Tree => Err(ActionError::git(
            op,
            path,
            "unexpected directory entry in a tree diff",
        )),
    }
}

/// Whether the path on disk holds exactly `expected`: the same bytes and the same kind. The
/// executable bit counts only under `file_mode` (`core.fileMode`), as in git. A missing path,
/// a directory, or a symlink where a file was is a mismatch; any other read failure is an
/// error, so its cause is not reported as an uncommitted change.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and the path when it cannot be inspected or read.
fn disk_matches(
    work_dir: &Path,
    path: &str,
    expected: (&[u8], EntryKind),
    file_mode: bool,
    op: &'static str,
) -> ActionResult<bool> {
    let (data, kind) = expected;
    let DiskEntry::Present {
        data: disk,
        kind: disk_kind,
    } = workfile::disk_entry(work_dir, path, op)?
    else {
        return Ok(false);
    };
    let kinds_match = match (disk_kind, kind) {
        (EntryKind::Link, EntryKind::Link) => true,
        (
            EntryKind::Blob | EntryKind::BlobExecutable,
            EntryKind::Blob | EntryKind::BlobExecutable,
        ) => !file_mode || disk_kind == kind,
        _ => false,
    };
    Ok(kinds_match && disk == data)
}
