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
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use super::commit::{
    blob_data, checked_out_branch, tree_of, whole_file_edit, work_dir, write_object,
};
use super::treebuild;
use super::workfile;
use super::{ActionError, ActionResult, Deleted, Identity, confirm, open, write_backup_ref};
use crate::{Action, Policy};

/// What a completed stash push produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashSaved {
    /// Hex id of the stash commit, now `stash@{0}`.
    pub oid: String,
    /// The reflog message, as `git stash list` will show it.
    pub message: String,
}

/// Records the changes at `paths` as a git-compatible stash entry, then restores those
/// files to their `HEAD` state on disk. `author` overrides the repository's own
/// `user.name`/`user.email`.
///
/// # Errors
/// [`ActionError::NotConfirmed`] when the plain confirm is missing,
/// [`ActionError::MissingIdentity`] without an identity, [`ActionError::Git`] when there is
/// no commit yet, nothing changed at `paths`, or an object cannot be written.
pub fn stash_push(
    repo_path: &Path,
    policy: &Policy,
    paths: &[String],
    message: Option<&str>,
    author: Option<Identity>,
    input: Option<&str>,
) -> ActionResult<StashSaved> {
    const OP: &str = "stash push";
    let repo = open(repo_path, OP, "refs/stash")?;
    let decision = policy.evaluate(&Action::StashPush { files: paths.len() });
    let _proof = confirm(decision, OP, "refs/stash", input)?;
    if paths.is_empty() {
        return Err(stash_error(OP, "nothing selected to stash"));
    }
    let (branch_ref, head_commit) = checked_out_branch(&repo)?;
    let head =
        head_commit.ok_or_else(|| stash_error(OP, "cannot stash before the first commit"))?;
    let identity = workfile::resolve_identity(&repo, author)?;
    let work_dir = work_dir(&repo)?;
    let base_tree = tree_of(&repo, head)?;

    let mut edits = std::collections::BTreeMap::new();
    for path in paths {
        let edit = whole_file_edit(&repo, &work_dir, Some(base_tree), path)?;
        treebuild::insert(&mut edits, path, edit)?;
    }
    let stash_tree = treebuild::rewrite(&repo, Some(base_tree), &edits)?
        .ok_or_else(|| stash_error(OP, "stashing every file would empty the tree"))?;
    if stash_tree == base_tree {
        return Err(stash_error(OP, "no changes to stash at the selected paths"));
    }

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
    let signature = gix::actor::Signature {
        name: identity.name.clone().into(),
        email: identity.email.clone().into(),
        time: gix::date::Time::now_local_or_utc(),
    };
    let index_commit = write_object(
        &repo,
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

    let existing = stash_target(&repo)?;
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: true,
                message: stash_message.clone().into(),
            },
            expected: match existing {
                Some(current) => PreviousValue::MustExistAndMatch(Target::Object(current)),
                None => PreviousValue::MustNotExist,
            },
            new: Target::Object(stash_commit),
        },
        name: stash_ref_name(OP)?,
        deref: false,
    };
    super::apply_edit_as(
        &repo,
        edit,
        OP,
        "refs/stash",
        &identity.name,
        &identity.email,
    )?;

    // Only now that the stash records everything: put the files back to HEAD state.
    for path in paths {
        match super::commit::tree_entry_at(&repo, Some(base_tree), path)? {
            Some(entry) => {
                let data = blob_data(&repo, path, entry.oid)?;
                workfile::restore_file(&work_dir, path, &data, entry.mode.into())?;
            }
            None => workfile::delete_file(&work_dir, path)?,
        }
    }
    Ok(StashSaved {
        oid: stash_commit.to_string(),
        message: stash_message,
    })
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
    let _proof = confirm(
        policy.evaluate(&Action::StashApply),
        OP,
        "refs/stash",
        input,
    )?;
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
    let (challenge, _confirmation) =
        confirm(policy.evaluate(&Action::StashPop), OP, "refs/stash", input)?;
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
    let decision = policy.evaluate(&Action::StashDrop {
        name: format!("stash@{{{index}}}"),
    });
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
        .map_err(|source| ActionError::Git {
            op,
            name: stash_commit.to_string(),
            source: Box::new(source),
        })?
        .parent_ids()
        .next()
        .map(gix::Id::detach)
        .ok_or_else(|| stash_error(op, "the stash commit has no base parent"))?;

    let core = rc_git_core::Repository::open(repo_path).map_err(|source| ActionError::Git {
        op,
        name: "repository".to_owned(),
        source: Box::new(source),
    })?;
    let changes = core
        .diff_trees(core_oid(base, op)?.into(), core_oid(stash_commit, op)?)
        .map_err(|source| ActionError::Git {
            op,
            name: "stash".to_owned(),
            source: Box::new(source),
        })?;
    let work_dir = work_dir(repo)?;

    // Pass 1: every touched path must be exactly at the base state; refuse before any write.
    for change in &changes {
        let path = change.path.to_string();
        let source_path = change.source_path.as_ref().map(ToString::to_string);
        match change.kind {
            rc_git_core::ChangeKind::Added | rc_git_core::ChangeKind::Copied => {
                require_absent(&work_dir, &path)?;
            }
            rc_git_core::ChangeKind::Modified => {
                require_base_content(repo, &work_dir, &path, change.old, op)?;
            }
            rc_git_core::ChangeKind::Deleted => {
                require_absent_or_base(repo, &work_dir, &path, change.old, op)?;
            }
            rc_git_core::ChangeKind::Renamed => {
                let source = source_path
                    .as_deref()
                    .ok_or_else(|| stash_error(op, "a rename without a source path"))?;
                require_absent_or_base(repo, &work_dir, source, change.old, op)?;
                require_absent(&work_dir, &path)?;
            }
        }
    }

    // Pass 2: write. Every check passed, so this only replays recorded state.
    for change in &changes {
        let path = change.path.to_string();
        match change.kind {
            rc_git_core::ChangeKind::Added
            | rc_git_core::ChangeKind::Copied
            | rc_git_core::ChangeKind::Modified => {
                restore_change(repo, &work_dir, &path, change, op)?;
            }
            rc_git_core::ChangeKind::Deleted => workfile::delete_file(&work_dir, &path)?,
            rc_git_core::ChangeKind::Renamed => {
                if let Some(source) = &change.source_path {
                    workfile::delete_file(&work_dir, &source.to_string())?;
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
        .ok_or_else(|| stash_error(op, "a written change carries no blob"))?;
    let oid = gix_oid(new, op)?;
    let data = blob_data(repo, path, oid)?;
    workfile::restore_file(work_dir, path, &data, entry_kind(change.entry))
}

fn entry_kind(kind: rc_git_core::EntryKind) -> gix::objs::tree::EntryKind {
    match kind {
        rc_git_core::EntryKind::Symlink => gix::objs::tree::EntryKind::Link,
        rc_git_core::EntryKind::ExecutableBlob => gix::objs::tree::EntryKind::BlobExecutable,
        rc_git_core::EntryKind::Blob
        | rc_git_core::EntryKind::Tree
        | rc_git_core::EntryKind::Submodule => gix::objs::tree::EntryKind::Blob,
    }
}

/// The path must not exist on disk at all.
fn require_absent(work_dir: &Path, path: &str) -> ActionResult<()> {
    let full = workfile::join_under(work_dir, path)?;
    match std::fs::symlink_metadata(&full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) | Err(_) => Err(ActionError::DirtyPath {
            path: path.to_owned(),
        }),
    }
}

/// The path's bytes on disk must equal the blob the stash recorded as its base.
fn require_base_content(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    base: Option<rc_git_core::Oid>,
    op: &'static str,
) -> ActionResult<()> {
    let dirty = || ActionError::DirtyPath {
        path: path.to_owned(),
    };
    let base = base.ok_or_else(|| stash_error(op, "a modification carries no base blob"))?;
    let expected = blob_data(repo, path, gix_oid(base, op)?)?;
    let full = workfile::join_under(work_dir, path)?;
    let meta = std::fs::symlink_metadata(&full).map_err(|_| dirty())?;
    let actual = if meta.file_type().is_symlink() {
        std::fs::read_link(&full)
            .map_err(|_| dirty())?
            .into_os_string()
            .into_encoded_bytes()
    } else if meta.is_dir() {
        return Err(dirty());
    } else {
        std::fs::read(&full).map_err(|_| dirty())?
    };
    if actual == expected {
        Ok(())
    } else {
        Err(dirty())
    }
}

fn require_absent_or_base(
    repo: &gix::Repository,
    work_dir: &Path,
    path: &str,
    base: Option<rc_git_core::Oid>,
    op: &'static str,
) -> ActionResult<()> {
    let full = workfile::join_under(work_dir, path)?;
    if let Err(error) = std::fs::symlink_metadata(&full)
        && error.kind() == std::io::ErrorKind::NotFound
    {
        return Ok(());
    }
    require_base_content(repo, work_dir, path, base, op)
}

/// Drops `stash@{index}`: backup ref, reflog line removal with re-chaining, and the
/// `refs/stash` update or deletion. The log file is rewritten last, under a lock, so the
/// spurious reflog line the ref update appends is erased again.
fn drop_inner(
    repo: &gix::Repository,
    index: usize,
    write_backup: bool,
    op: &'static str,
) -> ActionResult<Deleted> {
    let log_path = repo.common_dir().join("logs/refs/stash");
    let raw = std::fs::read(&log_path).map_err(|source| ActionError::Git {
        op,
        name: "refs/stash".to_owned(),
        source: Box::new(source),
    })?;
    let lines: Vec<&[u8]> = raw
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    if index >= lines.len() {
        return Err(stash_error(op, "no stash entry at that position"));
    }
    let position = lines.len() - 1 - index;
    let removed = lines[position];
    let (removed_old, removed_new) = line_oids(removed, op)?;

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

    let mut kept: Vec<Vec<u8>> = Vec::with_capacity(lines.len() - 1);
    for (current, line) in lines.iter().enumerate() {
        if current == position {
            continue;
        }
        if current == position + 1 {
            // The newer neighbor's old-oid re-chains to what the removed line pointed from.
            let (_, rest) = line.split_at(removed_old.to_string().len());
            let mut rebuilt = removed_old.to_string().into_bytes();
            rebuilt.extend_from_slice(rest);
            kept.push(rebuilt);
        } else {
            kept.push((*line).to_vec());
        }
    }

    let full_name = format!("stash@{{{index}}}");
    if kept.is_empty() {
        let edit = RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExistAndMatch(Target::Object(removed_new)),
                log: RefLog::AndReference,
            },
            name: stash_ref_name(op)?,
            deref: false,
        };
        super::apply_edit(repo, edit, op, "refs/stash")?;
        return Ok(Deleted {
            full_name,
            backup_ref: backup,
        });
    }

    if position == lines.len() - 1 {
        let (_, new_top) = line_oids(&kept[kept.len() - 1], op)?;
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "rc-git: stash drop".into(),
                },
                expected: PreviousValue::MustExistAndMatch(Target::Object(removed_new)),
                new: Target::Object(new_top),
            },
            name: stash_ref_name(op)?,
            deref: false,
        };
        super::apply_edit(repo, edit, op, "refs/stash")?;
    }

    rewrite_log(&log_path, &kept, op)?;
    Ok(Deleted {
        full_name,
        backup_ref: backup,
    })
}

/// Replaces the stash reflog with exactly `kept`, under a lock so nothing races git itself.
fn rewrite_log(log_path: &Path, kept: &[Vec<u8>], op: &'static str) -> ActionResult<()> {
    let log_error = |source: Box<dyn std::error::Error + Send + Sync + 'static>| ActionError::Git {
        op,
        name: "refs/stash".to_owned(),
        source,
    };
    let mut lock = gix::lock::File::acquire_to_update_resource(
        log_path,
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .map_err(|source| log_error(Box::new(source)))?;
    {
        use std::io::Write;
        for line in kept {
            lock.write_all(line)
                .and_then(|()| lock.write_all(b"\n"))
                .map_err(|source| log_error(Box::new(source)))?;
        }
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
        .ok_or_else(|| stash_error(op, "a reflog line has no old oid"))?;
    let new = fields
        .next()
        .and_then(|hex| ObjectId::from_hex(hex).ok())
        .ok_or_else(|| stash_error(op, "a reflog line has no new oid"))?;
    Ok((old, new))
}

/// The stash commit `stash@{index}` names, read from the reflog like git does.
fn stash_oid_at(repo: &gix::Repository, index: usize, op: &'static str) -> ActionResult<ObjectId> {
    let reference = repo
        .try_find_reference("refs/stash")
        .map_err(|source| ActionError::Git {
            op,
            name: "refs/stash".to_owned(),
            source: Box::new(source),
        })?
        .ok_or_else(|| stash_error(op, "the repository has no stash"))?;
    let mut platform = reference.log_iter();
    let lines = platform
        .all()
        .map_err(|source| ActionError::Git {
            op,
            name: "refs/stash".to_owned(),
            source: Box::new(source),
        })?
        .ok_or_else(|| stash_error(op, "the repository has no stash"))?;
    let mut oids: Vec<ObjectId> = Vec::new();
    for line in lines {
        let line = line.map_err(|source| ActionError::Git {
            op,
            name: "refs/stash".to_owned(),
            source: Box::new(source),
        })?;
        oids.push(line.new_oid());
    }
    oids.reverse();
    oids.get(index)
        .copied()
        .ok_or_else(|| stash_error(op, "no stash entry at that position"))
}

fn stash_target(repo: &gix::Repository) -> ActionResult<Option<ObjectId>> {
    const OP: &str = "stash push";
    let Some(reference) =
        repo.try_find_reference("refs/stash")
            .map_err(|source| ActionError::Git {
                op: OP,
                name: "refs/stash".to_owned(),
                source: Box::new(source),
            })?
    else {
        return Ok(None);
    };
    reference
        .clone()
        .into_fully_peeled_id()
        .map(|id| Some(id.detach()))
        .map_err(|source| ActionError::Git {
            op: OP,
            name: "refs/stash".to_owned(),
            source: Box::new(source),
        })
}

/// The first line of a commit's message, for `WIP on <branch>: <short> <subject>`.
fn commit_subject(repo: &gix::Repository, id: ObjectId) -> ActionResult<String> {
    const OP: &str = "stash push";
    let commit = repo.find_commit(id).map_err(|source| ActionError::Git {
        op: OP,
        name: id.to_string(),
        source: Box::new(source),
    })?;
    let message = commit.message_raw().map_err(|source| ActionError::Git {
        op: OP,
        name: id.to_string(),
        source: Box::new(source),
    })?;
    let first = message
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    Ok(String::from_utf8_lossy(first).into_owned())
}

fn stash_ref_name(op: &'static str) -> ActionResult<gix::refs::FullName> {
    "refs/stash"
        .try_into()
        .map_err(
            |source: gix::validate::reference::name::Error| ActionError::Git {
                op,
                name: "refs/stash".to_owned(),
                source: Box::new(source),
            },
        )
}

fn stash_error(op: &'static str, message: &'static str) -> ActionError {
    ActionError::Git {
        op,
        name: "refs/stash".to_owned(),
        source: message.into(),
    }
}

fn core_oid(id: ObjectId, op: &'static str) -> ActionResult<rc_git_core::Oid> {
    rc_git_core::Oid::from_bytes(id.as_slice())
        .ok_or_else(|| stash_error(op, "an oid did not convert"))
}

fn gix_oid(id: rc_git_core::Oid, op: &'static str) -> ActionResult<ObjectId> {
    ObjectId::try_from(id.as_bytes()).map_err(|_| stash_error(op, "an oid did not convert"))
}
