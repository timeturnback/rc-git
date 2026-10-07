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

use super::commit::{blob_data, update_index};
use super::{ActionError, ActionResult, workfile};

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
    let core = rc_git_core::Repository::open(repo_path).map_err(|source| ActionError::Git {
        op,
        name: "repository".to_owned(),
        source: Box::new(source),
    })?;
    let from = match from {
        Some(from) => Some(core_oid(from, op)?),
        None => None,
    };
    let to_oid = core_oid(to, op)?;
    let tree_changes = core
        .diff_trees(from, to_oid)
        .map_err(|source| ActionError::Git {
            op,
            name: to.to_string(),
            source: Box::new(source),
        })?;

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
            rc_git_core::ChangeKind::Copied => changes.push(PathChange {
                path,
                old: None,
                new: new_blob.map(|id| (id, kind)),
            }),
        }
    }
    Ok(changes)
}

/// Refuses the whole operation when any touched path is dirty on disk or staged in the index,
/// before anything is written.
///
/// # Errors
/// [`ActionError::DirtyPath`] for the first path that does not match `HEAD`,
/// [`ActionError::Git`] when the index or a blob cannot be read, or a path would cross a
/// symlink (one on disk, or one this plan writes).
pub(super) fn verify_clean(
    repo: &gix::Repository,
    work_dir: &Path,
    changes: &[PathChange],
    op: &'static str,
) -> ActionResult<()> {
    let index = gix::index::File::at_or_default(
        repo.index_path(),
        repo.object_hash(),
        false,
        gix::index::decode::Options::default(),
    )
    .map_err(|source| ActionError::Git {
        op,
        name: "index".to_owned(),
        source: Box::new(source),
    })?;
    refuse_paths_under_written_links(changes, op)?;
    let deleted_files = deleted_files_on_disk(work_dir, changes, op)?;
    for change in changes {
        let dirty = || ActionError::DirtyPath {
            op,
            path: change.path.clone(),
        };
        if let Some((old_blob, kind)) = change.old {
            let expected = blob_data(repo, op, &change.path, old_blob)?;
            if !disk_matches(work_dir, &change.path, &expected, kind, op)? {
                return Err(dirty());
            }
        } else {
            // The other side adds this path; anything already there is in the way.
            let full = workfile::join_under(work_dir, &change.path, op)?;
            if let Ok(meta) = std::fs::symlink_metadata(&full)
                && !deleted_files.contains(&file_id(&meta))
            {
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

/// Writes every change to the working tree, then updates the index for exactly those paths.
/// Deletions run before writes: on a case-insensitive volume a deleted `Readme.md` is the
/// same file as an added `README.md`, and deleting it afterwards would remove the new file.
/// Call [`verify_clean`] first.
///
/// # Errors
/// [`ActionError::Git`] naming `op` when a file or the index cannot be written.
pub(super) fn apply(
    repo: &gix::Repository,
    work_dir: &Path,
    changes: &[PathChange],
    op: &'static str,
) -> ActionResult<()> {
    let mut index_changes: Vec<(String, Option<(ObjectId, EntryKind)>)> = Vec::new();
    for change in changes.iter().filter(|change| change.new.is_none()) {
        workfile::delete_file(op, work_dir, &change.path)?;
        index_changes.push((change.path.clone(), None));
    }
    for change in changes {
        if let Some((oid, kind)) = change.new {
            let data = blob_data(repo, op, &change.path, oid)?;
            workfile::restore_file(op, work_dir, &change.path, &data, kind)?;
            index_changes.push((change.path.clone(), Some((oid, kind))));
        }
    }
    update_index(repo, op, work_dir, &index_changes)
}

fn core_oid(oid: ObjectId, op: &'static str) -> ActionResult<rc_git_core::Oid> {
    rc_git_core::Oid::from_bytes(oid.as_slice()).ok_or_else(|| ActionError::Git {
        op,
        name: oid.to_string(),
        source: "not a valid object id".into(),
    })
}

/// Maps the core's entry kind back to gix's, refusing what these writes cannot produce.
fn entry_kind(
    path: &str,
    kind: rc_git_core::EntryKind,
    op: &'static str,
) -> ActionResult<EntryKind> {
    match kind {
        rc_git_core::EntryKind::Blob => Ok(EntryKind::Blob),
        rc_git_core::EntryKind::ExecutableBlob => Ok(EntryKind::BlobExecutable),
        rc_git_core::EntryKind::Symlink => Ok(EntryKind::Link),
        rc_git_core::EntryKind::Submodule => Err(ActionError::Git {
            op,
            name: path.to_owned(),
            source: "changing a submodule is not supported; use git directly".into(),
        }),
        rc_git_core::EntryKind::Tree => Err(ActionError::Git {
            op,
            name: path.to_owned(),
            source: "unexpected directory entry in a tree diff".into(),
        }),
    }
}

/// Whether the file on disk holds exactly `data`, honoring the entry kind.
fn disk_matches(
    work_dir: &Path,
    path: &str,
    data: &[u8],
    kind: EntryKind,
    op: &'static str,
) -> ActionResult<bool> {
    let full = workfile::join_under(work_dir, path, op)?;
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
