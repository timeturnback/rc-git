//! The stash, hand-written in git's own format: a stash commit whose tree is the stashed
//! worktree state, with the base commit as first parent and an index commit as second, and
//! `refs/stash` whose reflog is the list. gix has no stash API; the interop tests prove the
//! real `git stash` accepts what is written here.
//!
//! Applying never merges: every touched path must sit exactly at the stash's base state, or
//! the whole apply is refused before anything is written. Dropping `stash@{n}` is git's
//! `reflog delete --updateref --rewrite` by hand: the kept reflog lines re-chain and are
//! rewritten byte-identically otherwise.

use std::path::Path;

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

/// Writes `stash@{index}`'s changes back to the working tree, keeping the entry.
///
/// # Errors
/// [`ActionError::DirtyPath`] when any touched path is not exactly at the stash's base
/// state — nothing is written then; [`ActionError::Git`] when the entry does not exist.
pub fn stash_apply(
    repo_path: &Path,
    policy: &Policy,
    index: usize,
    input: Option<&str>,
) -> ActionResult<()> {
    const OP: &str = "stash apply";
    let repo = open(repo_path, OP, "refs/stash")?;
    let _proof = confirm(plan_stash_apply(policy), OP, "refs/stash", input)?;
    apply_inner(&repo, repo_path, index, OP)
}

/// Applies `stash@{index}` and drops it once every file is restored. The policy writes a
/// backup ref first, so even a popped stash can be brought back.
///
/// # Errors
/// As [`stash_apply`]; a failed apply leaves the stash entry untouched.
pub fn stash_pop(
    repo_path: &Path,
    policy: &Policy,
    index: usize,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "stash pop";
    let repo = open(repo_path, OP, "refs/stash")?;
    let (challenge, _confirmation) = confirm(plan_stash_pop(policy), OP, "refs/stash", input)?;
    apply_inner(&repo, repo_path, index, OP)?;
    drop_inner(&repo, index, challenge.backup_ref, OP)
}

/// Deletes `stash@{index}` without applying it, behind the typed phrase `drop`, with a
/// backup ref under `refs/rc-git/backup/stash/`.
///
/// # Errors
/// [`ActionError::NotConfirmed`] when the typed input does not match,
/// [`ActionError::Git`] when the entry does not exist.
pub fn stash_drop(
    repo_path: &Path,
    policy: &Policy,
    index: usize,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "stash drop";
    let repo = open(repo_path, OP, "refs/stash")?;
    let decision = plan_stash_drop(policy, index);
    let (challenge, _confirmation) = confirm(decision, OP, "refs/stash", input)?;
    drop_inner(&repo, index, challenge.backup_ref, OP)
}

/// The restore half of apply and pop: refuse unless every touched path sits at the stash's
/// base state, then write the stash side of every change.
fn apply_inner(
    repo: &gix::Repository,
    repo_path: &Path,
    index: usize,
    op: &'static str,
) -> ActionResult<()> {
    let stash_commit = stash_oid_at(repo, index, op)?;
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

    // Pass 1: every touched path must be valid and exactly at the base state; refuse before
    // any write.
    for change in &changes {
        let path = change.path.to_string();
        let source_path = change.source_path.as_ref().map(ToString::to_string);
        let symlink = entry_kind(&path, change.entry, op)? == gix::objs::tree::EntryKind::Link;
        workfile::validate_path(op, &path, symlink)?;
        if let Some(source) = &source_path {
            workfile::validate_path(op, source, symlink)?;
        }
        match change.kind {
            rc_git_core::ChangeKind::Added => {
                require_absent(&work_dir, &path, op)?;
            }
            rc_git_core::ChangeKind::Modified => {
                require_base_content(repo, &work_dir, &path, change.old, op)?;
            }
            rc_git_core::ChangeKind::Deleted => {
                require_absent_or_base(repo, &work_dir, &path, change.old, op)?;
            }
            rc_git_core::ChangeKind::Renamed => {
                let source = source_path.as_deref().ok_or_else(|| {
                    ActionError::git(op, "refs/stash", "a rename without a source path")
                })?;
                require_absent_or_base(repo, &work_dir, source, change.old, op)?;
                require_absent(&work_dir, &path, op)?;
            }
        }
    }

    // Pass 2: write. Every check passed, so this only replays recorded state.
    for change in &changes {
        let path = change.path.to_string();
        match change.kind {
            rc_git_core::ChangeKind::Added | rc_git_core::ChangeKind::Modified => {
                restore_change(repo, &work_dir, &path, change, op)?;
            }
            rc_git_core::ChangeKind::Deleted => workfile::delete_file(op, &work_dir, &path)?,
            rc_git_core::ChangeKind::Renamed => {
                if let Some(source) = &change.source_path {
                    workfile::delete_file(op, &work_dir, &source.to_string())?;
                }
                restore_change(repo, &work_dir, &path, change, op)?;
            }
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

/// The path must not exist on disk at all.
fn require_absent(work_dir: &Path, path: &str, op: &'static str) -> ActionResult<()> {
    let full = workfile::join_under(work_dir, path, op)?;
    match std::fs::symlink_metadata(&full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ActionError::git(op, path, error)),
        Ok(_) => Err(ActionError::DirtyPath {
            op,
            path: path.to_owned(),
        }),
    }
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

/// Drops `stash@{index}`: backup ref, reflog line removal with re-chaining, and the
/// `refs/stash` update or deletion. `index` counts the reflog lines git can read, newest
/// first (see [`rc_git_core::stash_reflog_lines`]); unreadable lines stay as they are. The log
/// file is rewritten last, under a lock, so the spurious reflog line the ref update appends is
/// erased again.
fn drop_inner(
    repo: &gix::Repository,
    index: usize,
    write_backup: bool,
    op: &'static str,
) -> ActionResult<Deleted> {
    let log_path = repo.common_dir().join("logs/refs/stash");
    let raw = std::fs::read(&log_path).map_err(git_err(op, "refs/stash"))?;
    let valid = rc_git_core::stash_reflog_lines(&raw);
    let Some(target) = index
        .checked_add(1)
        .and_then(|from_end| valid.len().checked_sub(from_end))
    else {
        return Err(ActionError::git(
            op,
            "refs/stash",
            "no stash entry at that position",
        ));
    };
    let pieces: Vec<&[u8]> = raw.split(|byte| *byte == b'\n').collect();
    let position = valid[target].line;
    let (removed_old, removed_new) = line_oids(pieces[position], op)?;
    let newer = valid.get(target + 1).map(|line| line.line);

    let backup = if write_backup {
        Some(write_backup_ref(
            repo,
            "stash",
            &index.to_string(),
            removed_new,
        )?)
    } else {
        None
    };

    let full_name = stash_name(index);
    if valid.len() == 1 {
        let expected = PreviousValue::MustExistAndMatch(Target::Object(removed_new));
        let edit = super::delete_edit("refs/stash", expected, op)?;
        super::apply_edit(repo, edit, op, "refs/stash")?;
        return Ok(Deleted {
            full_name,
            backup_ref: backup,
        });
    }

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

    if newer.is_none() {
        let new_top = gix_oid(valid[target - 1].oid, op)?;
        let edit = super::update_edit(
            "refs/stash",
            PreviousValue::MustExistAndMatch(Target::Object(removed_new)),
            Target::Object(new_top),
            "rc-git: stash drop",
            op,
        )?;
        super::apply_edit(repo, edit, op, "refs/stash")?;
    }

    rewrite_log(&log_path, &kept.join(&b'\n'), op)?;
    Ok(Deleted {
        full_name,
        backup_ref: backup,
    })
}

/// Replaces the stash reflog with exactly `content`, under a lock so nothing races git itself.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
)]
fn rewrite_log(log_path: &Path, content: &[u8], op: &'static str) -> ActionResult<()> {
    let log_error = |source: Box<dyn std::error::Error + Send + Sync + 'static>| {
        ActionError::git(op, "refs/stash", source)
    };
    let mut lock = gix::lock::File::acquire_to_update_resource(
        log_path,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .map_err(|source| log_error(Box::new(source)))?;
    {
        use std::io::Write;
        lock.write_all(content)
            .map_err(|source| log_error(Box::new(source)))?;
    }
    lock.commit()
        .map_err(|source| log_error(Box::new(source)))?;
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

/// The stash commit `stash@{index}` names, read from the reflog like git does.
fn stash_oid_at(repo: &gix::Repository, index: usize, op: &'static str) -> ActionResult<ObjectId> {
    let reference = repo
        .try_find_reference("refs/stash")
        .map_err(git_err(op, "refs/stash"))?
        .ok_or_else(|| ActionError::git(op, "refs/stash", "the repository has no stash"))?;
    let mut platform = reference.log_iter();
    if platform.all().map_err(git_err(op, "refs/stash"))?.is_none() {
        return Err(ActionError::git(
            op,
            "refs/stash",
            "the repository has no stash",
        ));
    }
    let valid = rc_git_core::stash_reflog_lines(&platform.buf);
    let line = index
        .checked_add(1)
        .and_then(|from_end| valid.len().checked_sub(from_end))
        .and_then(|position| valid.get(position))
        .ok_or_else(|| ActionError::git(op, "refs/stash", "no stash entry at that position"))?;
    gix_oid(line.oid, op)
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
