//! Discard uncommitted changes: restore tracked files to their `HEAD` bytes, delete
//! untracked files, or splice selected hunks out of one file. Destructive under the policy,
//! so every entry point goes through [`authorize`] before touching disk. The challenge carries
//! no typed phrase: a plain confirm answers it.

use std::path::Path;

use gix::objs::tree::EntryKind;

use super::commit::{IndexChange, blob_data, disk_stat, tree_entry_at, verified_hunks, work_dir};
use super::workfile::{self, DiskEntry};
use super::write::{LockedIndex, Writer};
use super::{ActionError, ActionResult, HunkStamp, authorize, git_err, open};
use crate::{Action, Policy};

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

/// Discards every change at `paths`: a path tracked in `HEAD` gets its blob bytes written back to
/// disk, an untracked path is deleted, and `.git/index` gets the `HEAD` entry for each of them (or
/// loses it), so a staged change is discarded too. Every path is checked before the first write, so
/// a refused path leaves every other one untouched. The policy asks for a plain confirm, so `input`
/// may be `None`. Returns one outcome per path, in order.
///
/// # Errors
/// [`ActionError::Git`] when a path is one git refuses (`.git`, `.`, `..`, an empty segment,
/// a symlinked parent), names a directory in `HEAD` or on disk, names a submodule or a blob
/// missing from the object database, is untracked without a file of exactly that name on
/// disk, or can be neither restored nor deleted, and when another git
/// process holds `index.lock`. Only a failed write can stop the run part-way, after the paths
/// before it were handled; the index is then left as it was.
pub fn discard_files(
    repo_path: &Path,
    policy: &Policy,
    paths: &[String],
    input: Option<&str>,
) -> ActionResult<Vec<DiscardOutcome>> {
    let repo = open(repo_path, OP, "working tree")?;
    let action = Action::DiscardWorkingChanges { files: paths.len() };
    let proof = authorize(policy, &action, OP, "working tree", input)?;
    let writer = Writer::new(&repo, &proof);
    let work_dir = work_dir(&repo, OP)?;
    workfile::check_paths(&work_dir, paths.iter().map(String::as_str), OP)?;
    let head_tree = head_tree(&repo)?;
    let mut plan = Vec::with_capacity(paths.len());
    for path in paths {
        let step = if let Some(entry) = tree_entry_at(&repo, OP, head_tree, path)? {
            Plan::Restore(restorable(&repo, OP, path, entry)?)
        } else {
            workfile::require_exact_file_name(&work_dir, path, OP)?;
            Plan::Delete
        };
        plan.push((path, step));
    }
    let index = writer.lock_index(OP)?;
    let index_changes = write_plan(&writer, &work_dir, &plan, "the index was not updated", OP)?;
    writer.update_index(index, OP, &index_changes)?;
    let outcomes = plan
        .into_iter()
        .map(|(path, step)| DiscardOutcome {
            path: path.clone(),
            kind: match step {
                Plan::Restore(_) => DiscardKind::Restored,
                Plan::Delete => DiscardKind::Deleted,
            },
        })
        .collect();
    Ok(outcomes)
}

/// `entry` at `path`, checked for restoring before anything is written: a directory or a
/// submodule is refused, and the object's header is read, so a missing or mistyped blob fails
/// here rather than after earlier paths were written.
fn restorable(
    repo: &gix::Repository,
    op: &'static str,
    path: &str,
    entry: gix::objs::tree::Entry,
) -> ActionResult<gix::objs::tree::Entry> {
    if entry.mode.is_tree() {
        return Err(ActionError::git(
            op,
            path,
            "is a directory; select files instead",
        ));
    }
    if entry.mode.is_commit() {
        return Err(ActionError::git(
            op,
            path,
            "is a submodule; discard its changes in the submodule",
        ));
    }
    let header = repo.find_header(entry.oid).map_err(git_err(op, path))?;
    if header.kind() != gix::object::Kind::Blob {
        return Err(ActionError::git(
            op,
            path,
            format!("names {} {}, not a blob", header.kind(), entry.oid),
        ));
    }
    Ok(entry)
}

/// Carries out `plan` in order and returns the index change for each path. A failed write
/// stops the run, naming how many paths were already written and, as `unchanged`, what was
/// left as it was.
fn write_plan(
    writer: &Writer<'_>,
    work_dir: &Path,
    plan: &[(&String, Plan)],
    unchanged: &str,
    op: &'static str,
) -> ActionResult<Vec<IndexChange>> {
    let mut index_changes: Vec<IndexChange> = Vec::with_capacity(plan.len());
    for (path, step) in plan {
        let change = match step {
            Plan::Restore(entry) => restore_entry(writer, work_dir, path, entry, op),
            Plan::Delete => writer
                .delete_file(op, work_dir, path)
                .map(|()| ((*path).clone(), None)),
        };
        let change = change.map_err(|error| {
            workfile::stopped_part_way(error, index_changes.len(), plan.len(), unchanged)
        })?;
        index_changes.push(change);
    }
    Ok(index_changes)
}

/// Writes `entry`'s bytes and mode back to `path` and returns the index change that records
/// it: the blob id, kind, and the stat of the file just written.
fn restore_entry(
    writer: &Writer<'_>,
    work_dir: &Path,
    path: &str,
    entry: &gix::objs::tree::Entry,
    op: &'static str,
) -> ActionResult<IndexChange> {
    let kind = entry.mode.into();
    let data = blob_data(writer.repo(), op, path, entry.oid)?;
    writer.restore_file(op, work_dir, path, (&data, kind))?;
    let stat = disk_stat(work_dir, path, op);
    Ok((path.to_owned(), Some((entry.oid, kind, stat))))
}

/// Puts each of `paths` back to its state in `tree`, on disk and in `index`: a tracked file
/// gets its blob bytes and mode written back and its index entry set to that blob, an
/// untracked one is deleted and its index entry removed. Entries for other paths are kept.
/// Every entry is looked up and checked before the first write. `stash push` is the one
/// caller, so a failure says the stash already holds every change.
///
/// # Errors
/// [`ActionError::Git`] when an entry cannot be read or is a directory, a submodule or a
/// missing blob (before any write), a file can be neither restored nor deleted (naming how
/// many paths were already written), or the index cannot be written.
pub(super) fn restore_paths(
    writer: &Writer<'_>,
    work_dir: &Path,
    index: LockedIndex,
    (tree, paths): (Option<gix::ObjectId>, &[String]),
    op: &'static str,
) -> ActionResult<()> {
    const UNCHANGED: &str = "the stash holds every change, but the index was not updated";
    let repo = writer.repo();
    let mut plan = Vec::with_capacity(paths.len());
    for path in paths {
        let step = tree_entry_at(repo, op, tree, path)
            .and_then(|entry| match entry {
                Some(entry) => restorable(repo, op, path, entry).map(Plan::Restore),
                None => Ok(Plan::Delete),
            })
            .map_err(|error| workfile::stopped_part_way(error, 0, paths.len(), UNCHANGED))?;
        plan.push((path, step));
    }
    let index_changes = write_plan(writer, work_dir, &plan, UNCHANGED, op)?;
    writer.update_index(index, op, &index_changes)
}

/// Discards only `stamps` of `path`: the diff is recomputed, every stamp must match a
/// current hunk, and the file is rewritten with the unselected changes kept — that is, the
/// selected hunks' changes are gone. The policy asks for a plain confirm, so `input` may be
/// `None`.
///
/// # Errors
/// [`ActionError::NoHunksSelected`] when `stamps` is empty,
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
    let action = Action::DiscardWorkingChanges { files: 1 };
    let proof = authorize(policy, &action, OP, path, input)?;
    let mismatch = || ActionError::HunkMismatch {
        path: path.to_owned(),
    };
    if stamps.is_empty() {
        return Err(ActionError::NoHunksSelected {
            op: OP,
            path: path.to_owned(),
        });
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
    Writer::new(&repo, &proof).restore_file(OP, &work_dir, path, (&result, kind))
}

/// The tree `HEAD` points at; `None` on an unborn branch, where everything is untracked.
fn head_tree(repo: &gix::Repository) -> ActionResult<Option<gix::ObjectId>> {
    let Some(head) = super::head_commit(repo, OP)? else {
        return Ok(None);
    };
    super::commit::tree_of(repo, OP, head).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Action;

    #[test]
    fn restore_paths_checks_every_entry_before_writing_any() {
        let fx = rc_git_testkit::Fixture::standard();
        fx.write("alpha.txt", b"stashed edit\n");
        let alpha = fx.rev_parse("HEAD:alpha.txt");
        let ghost = fx.hash_blob(b"ghost\n");
        let tree = fx.mktree(
            format!("100644 blob {alpha}\talpha.txt\n100644 blob {ghost}\tghost.txt\n").as_bytes(),
        );
        fx.remove(&format!(".git/objects/{}/{}", &ghost[..2], &ghost[2..]));

        let repo = gix::open_opts(fx.path(), gix::open::Options::isolated()).unwrap();
        let proof = Policy::default()
            .authorize(&Action::StashPush { files: 2 }, None)
            .unwrap();
        let writer = Writer::new(&repo, &proof);
        let work_dir = work_dir(&repo, "stash push").unwrap();
        let index = writer.lock_index("stash push").unwrap();
        let tree = gix::ObjectId::from_hex(tree.as_bytes()).unwrap();
        let paths = ["alpha.txt".to_owned(), "ghost.txt".to_owned()];

        let err = restore_paths(
            &writer,
            &work_dir,
            index,
            (Some(tree), &paths),
            "stash push",
        )
        .unwrap_err();
        assert!(
            matches!(&err, ActionError::Git { op: "stash push", name, .. } if name == "ghost.txt"),
            "{err}"
        );
        assert!(
            err.to_string()
                .contains("stopped after writing 0 of 2 paths"),
            "{err}"
        );
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"stashed edit\n",
            "the path listed before the missing blob was not written"
        );
    }
}
