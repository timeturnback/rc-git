//! The stash, hand-written in git's own format: a stash commit whose tree is the stashed
//! worktree state, with the base commit as first parent and an index commit as second, and
//! `refs/stash` whose reflog is the list. gix has no stash API; the interop tests prove the
//! real `git stash` accepts what is written here.
//!
//! Applying never merges: every touched path must sit exactly at the stash's base state, or
//! the whole apply is refused before anything is written. Dropping `stash@{n}` is git's
//! `reflog delete --updateref --rewrite` by hand: the kept reflog lines re-chain and are
//! rewritten byte-identically otherwise.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use gix::ObjectId;
use gix::refs::Target;
use gix::refs::transaction::{Change, PreviousValue};

use super::commit::{
    LockedIndex, blob_data, checked_out_branch, refuse_case_variant_path, tree_entry_at, tree_of,
    whole_file_edit, work_dir, write_object,
};
use super::discard::restore_paths;
use super::treeapply::{core_oid, entry_kind};
use super::treebuild;
use super::workfile::{self, DiskEntry};
use super::{
    ActionError, ActionResult, Author, Deleted, Identity, confirm, git_err, open, plan_stash_apply,
    plan_stash_drop, plan_stash_pop, plan_stash_push, write_backup_ref,
};
use crate::Policy;

/// The reflog name git shows for the stash entry at `index`.
pub(super) fn stash_name(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// How a challenge names the stash entry at `index`: its reflog name and its message.
pub(super) fn stash_label(index: usize, message: &str) -> String {
    format!("{} \"{message}\"", stash_name(index))
}

/// The stash entry an apply, pop or drop acts on, read from the reflog.
struct StashTarget {
    oid: ObjectId,
    message: String,
}

/// What a completed stash push produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashSaved {
    /// Hex id of the stash commit, now `stash@{0}`.
    pub oid: String,
    /// The reflog message, as `git stash list` will show it.
    pub message: String,
}

/// Records the changes at `paths` as a git-compatible stash entry, then restores those
/// files to their `HEAD` state on disk and in `.git/index`, as `git stash push` does. `author` decides who is recorded, as [`Author`]
/// describes.
///
/// # Errors
/// [`ActionError::NotConfirmed`] when the plain confirm is missing,
/// [`ActionError::MissingIdentity`] without an identity, [`ActionError::Git`] when there is
/// no commit yet, nothing changed at `paths`, an object cannot be written, another git process
/// holds `index.lock`, or a merge, cherry-pick, revert or rebase is in progress.
pub fn stash_push(
    repo_path: &Path,
    policy: &Policy,
    paths: &[String],
    message: Option<&str>,
    author: Author,
    input: Option<&str>,
) -> ActionResult<StashSaved> {
    const OP: &str = "stash push";
    let repo = open(repo_path, OP, "refs/stash")?;
    super::refuse_unfinished_operation(&repo, OP)?;
    let _proof = confirm(
        plan_stash_push(policy, paths.len()),
        OP,
        "refs/stash",
        input,
    )?;
    let refused = |reason: &'static str| ActionError::git(OP, "refs/stash", reason);
    if paths.is_empty() {
        return Err(refused("nothing selected to stash"));
    }
    let (branch_ref, head_commit) = checked_out_branch(&repo, OP)?;
    let head = head_commit.ok_or_else(|| refused("cannot stash before the first commit"))?;
    let work_dir = work_dir(&repo, OP)?;
    workfile::check_paths(&work_dir, paths.iter().map(String::as_str), OP)?;
    let identity = workfile::resolve_identity(&repo, author)?;
    let base_tree = tree_of(&repo, OP, head)?;
    for path in paths {
        refuse_case_variant_path(&repo, &work_dir, Some(base_tree), path, OP)?;
        if tree_entry_at(&repo, OP, Some(base_tree), path)?.is_some_and(|e| e.mode.is_tree()) {
            return Err(ActionError::git(
                OP,
                path.clone(),
                "is a directory; select files instead",
            ));
        }
    }

    let mut edits = std::collections::BTreeMap::new();
    for path in paths {
        let edit = whole_file_edit(&repo, OP, &work_dir, Some(base_tree), path)?;
        treebuild::insert(&mut edits, path, edit)?;
    }
    let stash_tree = treebuild::rewrite(&repo, Some(base_tree), &edits)?
        .ok_or_else(|| refused("stashing every file would empty the tree"))?;
    if stash_tree == base_tree {
        return Err(refused("no changes to stash at the selected paths"));
    }
    let index = LockedIndex::acquire(&repo, OP)?;

    let branch = branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&branch_ref)
        .to_owned();
    let short = &head.to_string()[..7];
    let subject = commit_subject(&repo, head)?;
    let stash_message = match message {
        Some(text) if !text.trim().is_empty() => format!("On {branch}: {}", text.trim()),
        Some(_) | None => format!("WIP on {branch}: {short} {subject}"),
    };
    let reflog_message = one_line(&stash_message);
    let signature = gix::actor::Signature {
        name: identity.name.clone().into(),
        email: identity.email.clone().into(),
        time: gix::date::Time::now_local_or_utc(),
    };
    let index_commit = write_object(
        &repo,
        OP,
        &gix::objs::Commit {
            tree: base_tree,
            parents: [head].into_iter().collect(),
            author: signature.clone(),
            committer: signature.clone(),
            encoding: None,
            message: format!("index on {branch}: {short} {subject}\n").into(),
            extra_headers: Vec::new(),
        },
    )?;
    let stash_commit = write_object(
        &repo,
        OP,
        &gix::objs::Commit {
            tree: stash_tree,
            parents: [head, index_commit].into_iter().collect(),
            author: signature.clone(),
            committer: signature,
            encoding: None,
            message: format!("{stash_message}\n").into(),
            extra_headers: Vec::new(),
        },
    )?;

    write_stash_ref(&repo, stash_commit, &reflog_message, &identity)?;

    // Only now that the stash records everything: put the files back to HEAD state.
    restore_paths(&repo, &work_dir, index, Some(base_tree), paths, OP)?;
    Ok(StashSaved {
        oid: stash_commit.to_string(),
        message: reflog_message,
    })
}

/// `message` as one reflog line, the way git's `copy_reflog_msg` builds it: every whitespace
/// run, newlines included, becomes one space, and the ends are trimmed. The stash commit keeps
/// the full text; a newline in a reflog message would start a line git cannot parse.
fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Points `refs/stash` at `stash_commit`, adding a reflog line with `message`; the ref must
/// still be where it was read, so a concurrent push is refused rather than lost.
fn write_stash_ref(
    repo: &gix::Repository,
    stash_commit: ObjectId,
    message: &str,
    identity: &Identity,
) -> ActionResult<()> {
    const OP: &str = "stash push";
    let expected = match stash_target(repo)? {
        Some(current) => PreviousValue::MustExistAndMatch(Target::Object(current)),
        None => PreviousValue::MustNotExist,
    };
    let mut edit = super::update_edit(
        "refs/stash",
        expected,
        Target::Object(stash_commit),
        message,
        OP,
    )?;
    if let Change::Update { log, .. } = &mut edit.change {
        log.force_create_reflog = true;
    }
    super::apply_edit_as(
        repo,
        edit,
        OP,
        "refs/stash",
        &identity.name,
        &identity.email,
    )
}

/// Writes `stash@{index}`'s changes back to the working tree, keeping the entry. `expected`
/// is the stash commit the caller showed as `stash@{index}`; the apply is refused when the
/// entry at that position is now another one.
///
/// # Errors
/// [`ActionError::StaleRepository`] when `stash@{index}` is gone or is no longer `expected`;
/// [`ActionError::DirtyPath`] when any touched path is not exactly at the stash's base
/// state; [`ActionError::Git`] naming the untracked files when the entry was made with
/// `git stash -u` or `-a`, whose untracked side rc-git does not restore — nothing is written
/// in any of these cases.
pub fn stash_apply(
    repo_path: &Path,
    policy: &Policy,
    index: usize,
    expected: rc_git_core::Oid,
    input: Option<&str>,
) -> ActionResult<()> {
    const OP: &str = "stash apply";
    let repo = open(repo_path, OP, "refs/stash")?;
    let target = stash_entry_at(&repo, index, expected, OP)?;
    refuse_untracked_side(&repo, index, target.oid, OP)?;
    let _proof = confirm(plan_stash_apply(policy), OP, "refs/stash", input)?;
    apply_inner(&repo, repo_path, target.oid, OP)
}

/// Applies `stash@{index}` and drops it once every file is restored. The policy writes a
/// backup ref first, so even a popped stash can be brought back. `expected` is checked as
/// [`stash_apply`] checks it, and again against the reflog line the drop removes.
///
/// # Errors
/// As [`stash_apply`], untracked files included; a failed apply leaves the stash entry
/// untouched. When the apply
/// succeeds but the drop fails, [`ActionError::Git`] with op `"stash pop: drop"` says the
/// changes are already in the working tree, so the entry should be dropped, not popped again.
pub fn stash_pop(
    repo_path: &Path,
    policy: &Policy,
    index: usize,
    expected: rc_git_core::Oid,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "stash pop";
    let repo = open(repo_path, OP, "refs/stash")?;
    let target = stash_entry_at(&repo, index, expected, OP)?;
    refuse_untracked_side(&repo, index, target.oid, OP)?;
    let decision = plan_stash_pop(policy, index, &target.message);
    let (challenge, _confirmation) = confirm(decision, OP, "refs/stash", input)?;
    apply_inner(&repo, repo_path, target.oid, OP)?;
    drop_inner(&repo, index, target.oid, challenge.backup_ref, OP).map_err(|error| {
        ActionError::git(
            "stash pop: drop",
            stash_name(index),
            format!(
                "the changes were applied, but the entry was not dropped; drop it instead of \
                 popping again: {error}"
            ),
        )
    })
}

/// Deletes `stash@{index}` without applying it, behind the typed phrase `drop`, with a
/// backup ref under `refs/rc-git/backup/stash/`. `expected` is checked as [`stash_apply`]
/// checks it.
///
/// # Errors
/// [`ActionError::StaleRepository`] when `stash@{index}` is gone or is no longer `expected`,
/// [`ActionError::NotConfirmed`] when the typed input does not match.
pub fn stash_drop(
    repo_path: &Path,
    policy: &Policy,
    index: usize,
    expected: rc_git_core::Oid,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "stash drop";
    let repo = open(repo_path, OP, "refs/stash")?;
    let target = stash_entry_at(&repo, index, expected, OP)?;
    let decision = plan_stash_drop(policy, index, &target.message);
    let (challenge, _confirmation) = confirm(decision, OP, "refs/stash", input)?;
    drop_inner(&repo, index, target.oid, challenge.backup_ref, OP)
}

/// How many untracked paths a refusal names before it says "and more".
const UNTRACKED_SHOWN: usize = 5;

/// Refuses an entry made with `git stash -u` or `-a`: its untracked files live in a third
/// parent commit that apply does not restore, so applying it would bring back only part of
/// the stash, and popping it would then drop the rest. The refusal names the files and hands
/// the entry to git; at most [`UNTRACKED_SHOWN`] paths are read, so a stash of a whole
/// ignored tree costs a few tree objects.
fn refuse_untracked_side(
    repo: &gix::Repository,
    index: usize,
    stash_commit: ObjectId,
    op: &'static str,
) -> ActionResult<()> {
    let name = stash_name(index);
    let commit = repo
        .find_commit(stash_commit)
        .map_err(|source| ActionError::git(op, stash_commit.to_string(), source))?;
    let Some(untracked) = commit.parent_ids().nth(2).map(gix::Id::detach) else {
        return Ok(());
    };
    let tree = tree_of(repo, op, untracked)?;
    let mut paths = Vec::new();
    collect_paths(repo, tree, "", &mut paths, op)?;
    let mut listed = paths
        .iter()
        .take(UNTRACKED_SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > UNTRACKED_SHOWN {
        listed.push_str(" and more");
    }
    let (verb, done) = if op == "stash pop" {
        ("pop", "popped")
    } else {
        ("apply", "applied")
    };
    Err(ActionError::git(
        op,
        name.clone(),
        format!(
            "it also holds untracked files ({listed}) that rc-git cannot restore, so nothing \
             was {done}; run `git stash {verb} {name}` in a terminal"
        ),
    ))
}

/// Appends the file paths under `tree` to `paths`, depth first in tree order, until it holds
/// one more than [`UNTRACKED_SHOWN`].
fn collect_paths(
    repo: &gix::Repository,
    tree: ObjectId,
    prefix: &str,
    paths: &mut Vec<String>,
    op: &'static str,
) -> ActionResult<()> {
    let found = repo
        .find_tree(tree)
        .map_err(|source| ActionError::git(op, tree.to_string(), source))?;
    let decoded = found
        .decode()
        .map_err(|source| ActionError::git(op, tree.to_string(), source))?;
    for entry in &decoded.entries {
        if paths.len() > UNTRACKED_SHOWN {
            break;
        }
        let path = format!("{prefix}{}", entry.filename);
        if entry.mode.is_tree() {
            collect_paths(repo, entry.oid.to_owned(), &format!("{path}/"), paths, op)?;
        } else {
            paths.push(path);
        }
    }
    Ok(())
}

/// The restore half of apply and pop: refuse unless every touched path sits at the stash's
/// base state, then write the stash side of every change.
fn apply_inner(
    repo: &gix::Repository,
    repo_path: &Path,
    stash_commit: ObjectId,
    op: &'static str,
) -> ActionResult<()> {
    let base = repo
        .find_commit(stash_commit)
        .map_err(|source| ActionError::git(op, stash_commit.to_string(), source))?
        .parent_ids()
        .next()
        .map(gix::Id::detach)
        .ok_or_else(|| ActionError::git(op, "refs/stash", "the stash commit has no base parent"))?;

    let core = rc_git_core::Repository::open(repo_path).map_err(git_err(op, "repository"))?;
    let changes = core
        .diff_trees_without_renames(core_oid(base, op)?.into(), core_oid(stash_commit, op)?)
        .map_err(git_err(op, "stash"))?;
    let work_dir = work_dir(repo, op)?;
    let removed = removed_paths(&changes);
    check_changes(repo, &work_dir, &changes, &removed, op)?;
    write_changes(repo, &work_dir, &changes, op)
}

/// Every path the stash removes from the working tree: deletions and rename sources.
fn removed_paths(changes: &[rc_git_core::TreeChange]) -> BTreeSet<String> {
    let mut removed = BTreeSet::new();
    for change in changes {
        match change.kind {
            rc_git_core::ChangeKind::Deleted => {
                removed.insert(change.path.to_string());
            }
            rc_git_core::ChangeKind::Renamed => {
                if let Some(source) = &change.source_path {
                    removed.insert(source.to_string());
                }
            }
            rc_git_core::ChangeKind::Added | rc_git_core::ChangeKind::Modified => {}
        }
    }
    removed
}

/// Pass 1: every touched path must be valid and exactly at the base state; refuse before
/// any write.
fn check_changes(
    repo: &gix::Repository,
    work_dir: &Path,
    changes: &[rc_git_core::TreeChange],
    removed: &BTreeSet<String>,
    op: &'static str,
) -> ActionResult<()> {
    for change in changes {
        let path = change.path.to_string();
        let source_path = change.source_path.as_ref().map(ToString::to_string);
        let symlink = entry_kind(&path, change.entry, op)? == gix::objs::tree::EntryKind::Link;
        workfile::validate_path(op, &path, symlink)?;
        if let Some(source) = &source_path {
            workfile::validate_path(op, source, symlink)?;
        }
        match change.kind {
            rc_git_core::ChangeKind::Added => {
                require_absent(work_dir, &path, removed, op)?;
            }
            rc_git_core::ChangeKind::Modified => {
                require_base_content(repo, work_dir, &path, change.old, op)?;
            }
            rc_git_core::ChangeKind::Deleted => {
                require_absent_or_base(repo, work_dir, &path, change.old, op)?;
            }
            rc_git_core::ChangeKind::Renamed => {
                let source = source_path.as_deref().ok_or_else(|| {
                    ActionError::git(op, "refs/stash", "a rename without a source path")
                })?;
                require_absent_or_base(repo, work_dir, source, change.old, op)?;
                require_absent(work_dir, &path, removed, op)?;
            }
        }
    }
    Ok(())
}

/// Pass 2: write. Every check passed, so this only replays recorded state. Removals run
/// first, so a file the stash turns into a directory (`x` to `x/y`), or a directory it turns
/// into a file, is out of the way before the new entry is written.
fn write_changes(
    repo: &gix::Repository,
    work_dir: &Path,
    changes: &[rc_git_core::TreeChange],
    op: &'static str,
) -> ActionResult<()> {
    for change in changes {
        match change.kind {
            rc_git_core::ChangeKind::Deleted => {
                workfile::delete_file(op, work_dir, &change.path.to_string())?;
            }
            rc_git_core::ChangeKind::Renamed => {
                if let Some(source) = &change.source_path {
                    workfile::delete_file(op, work_dir, &source.to_string())?;
                }
            }
            rc_git_core::ChangeKind::Added | rc_git_core::ChangeKind::Modified => {}
        }
    }
    for change in changes {
        match change.kind {
            rc_git_core::ChangeKind::Added
            | rc_git_core::ChangeKind::Modified
            | rc_git_core::ChangeKind::Renamed => {
                restore_change(repo, work_dir, &change.path.to_string(), change, op)?;
            }
            rc_git_core::ChangeKind::Deleted => {}
        }
    }
    Ok(())
}

fn restore_change(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    change: &rc_git_core::TreeChange,
    op: &'static str,
) -> ActionResult<()> {
    let new = change
        .new
        .ok_or_else(|| ActionError::git(op, "refs/stash", "a written change carries no blob"))?;
    let oid = gix_oid(new, op)?;
    let data = blob_data(repo, op, path, oid)?;
    let kind = entry_kind(path, change.entry, op)?;
    workfile::restore_file(op, work_dir, path, &data, kind)
}

/// The path must not exist on disk, unless it is in the way of a type change the stash makes
/// and the apply removes it first: a parent that is a `removed` file (`x` for `x/y`), or a
/// directory holding nothing but `removed` files (`x/y` for `x`). Each removed file is
/// checked against the base by its own deletion.
fn require_absent(
    work_dir: &Path,
    path: &str,
    removed: &BTreeSet<String>,
    op: &'static str,
) -> ActionResult<()> {
    let full = workfile::join_under(work_dir, path, op)?;
    match std::fs::symlink_metadata(&full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotADirectory
                && has_removed_parent(path, removed) =>
        {
            Ok(())
        }
        Err(error) => Err(ActionError::git(op, path, error)),
        Ok(meta) if meta.is_dir() => require_only_removed(work_dir, path, removed, op),
        Ok(_) => Err(ActionError::DirtyPath {
            op,
            path: path.to_owned(),
        }),
    }
}

fn has_removed_parent(path: &str, removed: &BTreeSet<String>) -> bool {
    path.rmatch_indices('/')
        .any(|(end, _)| removed.contains(&path[..end]))
}

/// The directory `dir` must hold only files in `removed`, so deleting them empties it. The
/// walk stops at the first other entry, so it reads at most one entry per removed file.
fn require_only_removed(
    work_dir: &Path,
    dir: &str,
    removed: &BTreeSet<String>,
    op: &'static str,
) -> ActionResult<()> {
    let full = workfile::join_under(work_dir, dir, op)?;
    let entries = std::fs::read_dir(&full).map_err(git_err(op, dir))?;
    for entry in entries {
        let entry = entry.map_err(git_err(op, dir))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(ActionError::DirtyPath {
                op,
                path: dir.to_owned(),
            });
        };
        let child = format!("{dir}/{name}");
        let file_type = entry.file_type().map_err(git_err(op, &child))?;
        if file_type.is_dir() {
            require_only_removed(work_dir, &child, removed, op)?;
        } else if !removed.contains(&child) {
            return Err(ActionError::DirtyPath { op, path: child });
        }
    }
    Ok(())
}

/// The path's bytes on disk must equal the blob the stash recorded as its base. A missing
/// path or a directory is dirty; any other read failure is an error naming the path.
fn require_base_content(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    base: Option<rc_git_core::Oid>,
    op: &'static str,
) -> ActionResult<()> {
    let dirty = || ActionError::DirtyPath {
        op,
        path: path.to_owned(),
    };
    let base = base
        .ok_or_else(|| ActionError::git(op, "refs/stash", "a modification carries no base blob"))?;
    let expected = blob_data(repo, op, path, gix_oid(base, op)?)?;
    match workfile::disk_entry(work_dir, path, op)? {
        DiskEntry::Present { data, .. } if data == expected => Ok(()),
        DiskEntry::Present { .. } | DiskEntry::Absent | DiskEntry::Directory => Err(dirty()),
    }
}

fn require_absent_or_base(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    base: Option<rc_git_core::Oid>,
    op: &'static str,
) -> ActionResult<()> {
    let full = workfile::join_under(work_dir, path, op)?;
    match std::fs::symlink_metadata(&full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ActionError::git(op, path, error)),
        Ok(_) => require_base_content(repo, work_dir, path, base, op),
    }
}

/// How long a drop waits for `logs/refs/stash.lock`: gix's default ref-file lock timeout
/// (`core.filesRefLockTimeout`), so a lock another writer holds briefly does not fail the drop.
const LOG_LOCK_WAIT: Duration = Duration::from_millis(100);

/// The reflog edit a drop makes, decided from one read of `logs/refs/stash`.
struct DropPlan {
    /// The stash commit the removed line records.
    removed: ObjectId,
    /// The commit `refs/stash` moves to when the newest entry is dropped.
    new_top: Option<ObjectId>,
    /// The commit `refs/stash` names once the ref update, if any, is done.
    top: ObjectId,
    /// The dropped entry is the only one: `refs/stash` and its log are deleted.
    last: bool,
    /// The reflog without the removed line, its newer neighbor re-chained.
    kept: Vec<u8>,
}

/// Drops `stash@{index}`: backup ref, reflog line removal with re-chaining, and the
/// `refs/stash` update or deletion. `index` counts the reflog lines git can read, newest
/// first (see [`rc_git_core::stash_reflog_lines`]); unreadable lines stay as they are. The
/// line removed must still record `expected`, or nothing is written.
///
/// `logs/refs/stash.lock` is taken before the log is read and before anything is written, so
/// a lock held elsewhere fails the drop with nothing changed, and `git reflog expire` or
/// `git stash drop` (which take that lock) cannot interleave. `git stash push` appends while
/// holding `refs/stash.lock` instead, which a gix transaction cannot share across the read
/// and the rewrite, so it is not excluded: the log is re-read before the rewrite and a drop
/// that would erase a line it did not plan from refuses with
/// [`ActionError::StaleRepository`]. The rewrite also erases the spurious reflog line the
/// ref update appends.
fn drop_inner(
    repo: &gix::Repository,
    index: usize,
    expected: ObjectId,
    write_backup: bool,
    op: &'static str,
) -> ActionResult<Deleted> {
    let log_path = repo.common_dir().join("logs/refs/stash");
    let lock = lock_log(&log_path, op)?;
    let raw = std::fs::read(&log_path).map_err(git_err(op, "refs/stash"))?;
    let plan = plan_drop(&raw, index, expected, op)?;

    let backup = if write_backup {
        Some(write_backup_ref(
            repo,
            "stash",
            &index.to_string(),
            plan.removed,
        )?)
    } else {
        None
    };
    let deleted = Deleted {
        full_name: stash_name(index),
        backup_ref: backup,
    };

    let current = PreviousValue::MustExistAndMatch(Target::Object(plan.removed));
    if plan.last {
        let edit = super::delete_edit("refs/stash", current, op)?;
        super::apply_edit(repo, edit, op, "refs/stash")?;
        return Ok(deleted);
    }
    if let Some(new_top) = plan.new_top {
        let edit = super::update_edit(
            "refs/stash",
            current,
            Target::Object(new_top),
            "rc-git: stash drop",
            op,
        )?;
        super::apply_edit(repo, edit, op, "refs/stash")?;
    }
    require_log_unchanged(repo, &log_path, &raw, &plan, op)?;
    commit_log(lock, &plan.kept, op)?;
    Ok(deleted)
}

/// Finds the line `stash@{index}` names in `raw` and builds the log without it.
fn plan_drop(
    raw: &[u8],
    index: usize,
    expected: ObjectId,
    op: &'static str,
) -> ActionResult<DropPlan> {
    let valid = rc_git_core::stash_reflog_lines(raw);
    let Some(target) = index
        .checked_add(1)
        .and_then(|from_end| valid.len().checked_sub(from_end))
    else {
        return Err(ActionError::StaleRepository { op });
    };
    let pieces: Vec<&[u8]> = raw.split(|byte| *byte == b'\n').collect();
    let position = valid[target].line;
    let (removed_old, removed) = line_oids(pieces[position], op)?;
    if removed != expected {
        return Err(ActionError::StaleRepository { op });
    }
    let newer = valid.get(target + 1).map(|line| line.line);
    let new_top = match (newer, target.checked_sub(1)) {
        (None, Some(below)) => Some(gix_oid(valid[below].oid, op)?),
        (Some(_), _) | (None, None) => None,
    };
    let top = match (new_top, valid.last()) {
        (Some(new_top), _) => new_top,
        (None, Some(newest)) => gix_oid(newest.oid, op)?,
        (None, None) => return Err(ActionError::StaleRepository { op }),
    };

    let mut kept: Vec<Vec<u8>> = Vec::with_capacity(pieces.len() - 1);
    for (current, line) in pieces.iter().enumerate() {
        if current == position {
            continue;
        }
        if Some(current) == newer {
            // The newer neighbor's old-oid re-chains to what the removed line pointed from.
            let (newer_old, _) = line_oids(line, op)?;
            let (_, rest) = line.split_at(newer_old.to_string().len());
            let mut rebuilt = removed_old.to_string().into_bytes();
            rebuilt.extend_from_slice(rest);
            kept.push(rebuilt);
        } else {
            kept.push((*line).to_vec());
        }
    }
    Ok(DropPlan {
        removed,
        new_top,
        top,
        last: valid.len() == 1,
        kept: kept.join(&b'\n'),
    })
}

/// The log must still be the `raw` bytes the drop was planned from, plus at most the one line
/// the drop's own ref update appended, and `refs/stash` must name the top the drop expects:
/// anything else is a writer the log lock does not exclude, whose line the rewrite would erase.
fn require_log_unchanged(
    repo: &gix::Repository,
    log_path: &Path,
    raw: &[u8],
    plan: &DropPlan,
    op: &'static str,
) -> ActionResult<()> {
    let stale = || ActionError::StaleRepository { op };
    let now = std::fs::read(log_path).map_err(git_err(op, "refs/stash"))?;
    let tail = now.strip_prefix(raw).ok_or_else(stale)?;
    let own_line = match plan.new_top {
        None => tail.is_empty(),
        Some(new_top) => is_own_drop_line(tail, plan.removed, new_top),
    };
    if !own_line {
        return Err(stale());
    }
    let reference = repo
        .try_find_reference("refs/stash")
        .map_err(git_err(op, "refs/stash"))?
        .ok_or_else(stale)?;
    if reference.target().try_id() != Some(plan.top.as_ref()) {
        return Err(stale());
    }
    Ok(())
}

/// `tail` is exactly one reflog line moving `refs/stash` from `from` to `to`.
fn is_own_drop_line(tail: &[u8], from: ObjectId, to: ObjectId) -> bool {
    let Some(line) = tail.strip_suffix(b"\n") else {
        return false;
    };
    !line.contains(&b'\n') && line_oids(line, "stash drop").is_ok_and(|oids| oids == (from, to))
}

/// Takes `logs/refs/stash.lock`, waiting up to [`LOG_LOCK_WAIT`] for another holder.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
)]
fn lock_log(log_path: &Path, op: &'static str) -> ActionResult<gix::lock::File> {
    gix::lock::File::acquire_to_update_resource(
        log_path,
        gix::lock::acquire::Fail::AfterDurationWithBackoff(LOG_LOCK_WAIT),
        None,
    )
    .map_err(|source| ActionError::git(op, "refs/stash", source))
}

/// Replaces the stash reflog with exactly `content` by committing the held lock.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
)]
fn commit_log(mut lock: gix::lock::File, content: &[u8], op: &'static str) -> ActionResult<()> {
    {
        use std::io::Write;
        lock.write_all(content)
            .map_err(|source| ActionError::git(op, "refs/stash", source))?;
    }
    lock.commit()
        .map_err(|source| ActionError::git(op, "refs/stash", source))?;
    Ok(())
}

/// The first two whitespace-separated fields of a reflog line: the old and new oids.
fn line_oids(line: &[u8], op: &'static str) -> ActionResult<(ObjectId, ObjectId)> {
    let mut fields = line.split(|byte| *byte == b' ');
    let old = fields
        .next()
        .and_then(|hex| ObjectId::from_hex(hex).ok())
        .ok_or_else(|| ActionError::git(op, "refs/stash", "a reflog line has no old oid"))?;
    let new = fields
        .next()
        .and_then(|hex| ObjectId::from_hex(hex).ok())
        .ok_or_else(|| ActionError::git(op, "refs/stash", "a reflog line has no new oid"))?;
    Ok((old, new))
}

/// The entry `stash@{index}` names, read from the reflog like git does. The caller showed
/// `expected` at that position, so a missing entry or a different commit there means the
/// stash changed since the list was built: [`ActionError::StaleRepository`].
fn stash_entry_at(
    repo: &gix::Repository,
    index: usize,
    expected: rc_git_core::Oid,
    op: &'static str,
) -> ActionResult<StashTarget> {
    let stale = || ActionError::StaleRepository { op };
    let reference = repo
        .try_find_reference("refs/stash")
        .map_err(git_err(op, "refs/stash"))?
        .ok_or_else(stale)?;
    let mut platform = reference.log_iter();
    if platform.all().map_err(git_err(op, "refs/stash"))?.is_none() {
        return Err(stale());
    }
    let valid = rc_git_core::stash_reflog_lines(&platform.buf);
    let line = index
        .checked_add(1)
        .and_then(|from_end| valid.len().checked_sub(from_end))
        .and_then(|position| valid.get(position))
        .ok_or_else(stale)?;
    if line.oid != expected {
        return Err(stale());
    }
    Ok(StashTarget {
        oid: gix_oid(line.oid, op)?,
        message: line.message.clone(),
    })
}

fn stash_target(repo: &gix::Repository) -> ActionResult<Option<ObjectId>> {
    const OP: &str = "stash push";
    let Some(reference) = repo
        .try_find_reference("refs/stash")
        .map_err(git_err(OP, "refs/stash"))?
    else {
        return Ok(None);
    };
    reference
        .clone()
        .into_fully_peeled_id()
        .map(|id| Some(id.detach()))
        .map_err(git_err(OP, "refs/stash"))
}

/// The first line of a commit's message, for `WIP on <branch>: <short> <subject>`.
fn commit_subject(repo: &gix::Repository, id: ObjectId) -> ActionResult<String> {
    const OP: &str = "stash push";
    let commit = repo
        .find_commit(id)
        .map_err(|source| ActionError::git(OP, id.to_string(), source))?;
    let message = commit
        .message_raw()
        .map_err(|source| ActionError::git(OP, id.to_string(), source))?;
    let first = message
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    Ok(String::from_utf8_lossy(first).into_owned())
}

fn gix_oid(id: rc_git_core::Oid, op: &'static str) -> ActionResult<ObjectId> {
    ObjectId::try_from(id.as_bytes())
        .map_err(|_| ActionError::git(op, "refs/stash", "an oid did not convert"))
}
