//! Commit selected working-tree changes: blobs from disk, a rewritten tree, a commit
//! object, the branch ref through [`apply_edit`](super::apply_edit), and an index update so
//! terminal `git status` agrees afterwards. Raw bytes throughout: gitattributes filters and
//! eol conversion are not applied, matching how the core compares and diffs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gix::ObjectId;
use gix::bstr::{BString, ByteSlice};
use gix::objs::tree::EntryKind;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use super::treebuild::{self, Node, PathEdit};
use super::workfile;
use super::{ActionError, ActionResult, FileSelection, HunkStamp, Identity, confirm, open};
use crate::{Action, Policy};

/// What a completed commit produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// Hex id of the new commit.
    pub oid: String,
    /// Short name of the branch that advanced.
    pub branch: String,
}

const OP: &str = "commit";

/// Commits `selections` to the checked-out branch with `message`. `expected_head` is the
/// commit the change list was built against — `None` for an unborn branch — and the commit
/// is refused when `HEAD` moved away from it. `author` overrides the repository's own
/// `user.name`/`user.email`; without either the commit is refused. Whole-file selections
/// commit the bytes on disk; hunk selections are re-validated against a fresh diff and
/// spliced onto the tree side.
///
/// # Errors
/// [`ActionError::DetachedHead`], [`ActionError::StaleRepository`],
/// [`ActionError::MissingIdentity`], [`ActionError::HunkMismatch`] as described, and
/// [`ActionError::Git`] when an object cannot be read or written.
pub fn commit(
    repo_path: &Path,
    policy: &Policy,
    message: &str,
    author: Option<Identity>,
    expected_head: Option<&str>,
    selections: &[FileSelection],
    input: Option<&str>,
) -> ActionResult<Committed> {
    let repo = open(repo_path, OP, "HEAD")?;
    let decision = policy.evaluate(&Action::Commit {
        files: selections.len(),
    });
    let _proof = confirm(decision, OP, "HEAD", input)?;
    let message = normalized_message(message)?;
    let (branch_ref, head_commit) = checked_out_branch(&repo)?;
    let current = head_commit.map(|id| id.to_string());
    if current.as_deref() != expected_head {
        return Err(ActionError::StaleRepository { op: OP });
    }
    if selections.is_empty() {
        return Err(git_error("nothing selected to commit"));
    }
    let identity = workfile::resolve_identity(&repo, author)?;
    let work_dir = work_dir(&repo)?;
    let base_tree = head_commit.map(|id| tree_of(&repo, id)).transpose()?;

    let mut edits: BTreeMap<BString, Node> = BTreeMap::new();
    let mut index_changes: Vec<(String, Option<(ObjectId, EntryKind)>)> = Vec::new();
    for selection in selections {
        let (path, edit) = match selection {
            FileSelection::Whole { path } => {
                (path, whole_file_edit(&repo, &work_dir, base_tree, path)?)
            }
            FileSelection::Hunks { path, hunks } => (
                path,
                spliced_edit(&repo, repo_path, &work_dir, base_tree, path, hunks)?,
            ),
        };
        index_changes.push((
            path.clone(),
            match &edit {
                PathEdit::Put { oid, kind } => Some((*oid, *kind)),
                PathEdit::Remove => None,
            },
        ));
        treebuild::insert(&mut edits, path, edit)?;
    }

    let new_tree = match treebuild::rewrite(&repo, base_tree, &edits)? {
        Some(oid) => oid,
        None => write_object(&repo, &gix::objs::Tree::empty())?,
    };
    if Some(new_tree) == base_tree {
        return Err(git_error("the selection matches HEAD; nothing to commit"));
    }

    let signature = gix::actor::Signature {
        name: identity.name.into(),
        email: identity.email.into(),
        time: gix::date::Time::now_local_or_utc(),
    };
    let commit_object = gix::objs::Commit {
        tree: new_tree,
        parents: head_commit.into_iter().collect(),
        author: signature.clone(),
        committer: signature,
        encoding: None,
        message: message.into(),
        extra_headers: Vec::new(),
    };
    let commit_oid = write_object(&repo, &commit_object)?;

    let expected = match head_commit {
        Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
        None => PreviousValue::MustNotExist,
    };
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "rc-git: commit".into(),
            },
            expected,
            new: Target::Object(commit_oid),
        },
        name: branch_ref.as_str().try_into().map_err(
            |source: gix::validate::reference::name::Error| ActionError::Git {
                op: OP,
                name: branch_ref.clone(),
                source: Box::new(source),
            },
        )?,
        deref: false,
    };
    super::apply_edit(&repo, edit, OP, &branch_ref)?;
    update_index(&repo, &work_dir, &index_changes)?;

    Ok(Committed {
        oid: commit_oid.to_string(),
        branch: branch_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(&branch_ref)
            .to_owned(),
    })
}

/// The full ref name `HEAD` points at and the commit it resolves to; `None` for an unborn
/// branch. A detached `HEAD` is refused: v1 commits to a branch or not at all.
pub(super) fn checked_out_branch(
    repo: &gix::Repository,
) -> ActionResult<(String, Option<ObjectId>)> {
    let head_name = repo
        .head_name()
        .map_err(|source| ActionError::Git {
            op: OP,
            name: "HEAD".to_owned(),
            source: Box::new(source),
        })?
        .ok_or(ActionError::DetachedHead { op: "commit" })?;
    let commit = repo.head_id().ok().map(gix::Id::detach);
    Ok((head_name.as_bstr().to_string(), commit))
}

pub(super) fn work_dir(repo: &gix::Repository) -> ActionResult<PathBuf> {
    repo.workdir()
        .map(Path::to_path_buf)
        .ok_or_else(|| git_error("the repository is bare and has no working tree"))
}

pub(super) fn tree_of(repo: &gix::Repository, commit: ObjectId) -> ActionResult<ObjectId> {
    let commit = repo
        .find_commit(commit)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: commit.to_string(),
            source: Box::new(source),
        })?;
    commit
        .tree_id()
        .map(gix::Id::detach)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: commit.id.to_string(),
            source: Box::new(source),
        })
}

/// The edit for committing `path` exactly as disk has it: an addition or replacement from
/// the file's bytes, or a removal when the file is gone.
pub(super) fn whole_file_edit(
    repo: &gix::Repository,
    work_dir: &Path,
    base_tree: Option<ObjectId>,
    path: &str,
) -> ActionResult<PathEdit> {
    let full = workfile::join_under(work_dir, path)?;
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if tree_entry_at(repo, base_tree, path)?.is_none() {
                return Err(git_error_for(path, "does not exist on disk or in HEAD"));
            }
            return Ok(PathEdit::Remove);
        }
        Err(error) => {
            return Err(ActionError::Git {
                op: OP,
                name: path.to_owned(),
                source: Box::new(error),
            });
        }
    };
    let (data, kind) = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&full).map_err(|source| ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: Box::new(source),
        })?;
        (
            target.into_os_string().into_encoded_bytes(),
            EntryKind::Link,
        )
    } else if meta.is_dir() {
        return Err(git_error_for(path, "is a directory; select files instead"));
    } else {
        let data = std::fs::read(&full).map_err(|source| ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: Box::new(source),
        })?;
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        };
        (
            data,
            if executable {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            },
        )
    };
    let oid = write_blob(repo, path, &data)?;
    Ok(PathEdit::Put { oid, kind })
}

/// The edit for committing only `stamps` of `path`: the diff is recomputed with the same
/// code that built the UI's hunks, every stamp must match a current hunk exactly, and the
/// selected new-side ranges are spliced onto the HEAD blob.
fn spliced_edit(
    repo: &gix::Repository,
    repo_path: &Path,
    work_dir: &Path,
    base_tree: Option<ObjectId>,
    path: &str,
    stamps: &[HunkStamp],
) -> ActionResult<PathEdit> {
    if stamps.is_empty() {
        return Err(ActionError::HunkMismatch {
            path: path.to_owned(),
        });
    }
    let entry = tree_entry_at(repo, base_tree, path)?.ok_or_else(|| ActionError::HunkMismatch {
        path: path.to_owned(),
    })?;
    if !entry.mode.is_blob() {
        return Err(ActionError::HunkMismatch {
            path: path.to_owned(),
        });
    }
    verify_stamps(repo_path, path, entry.oid, stamps)?;

    let old = blob_data(repo, path, entry.oid)?;
    let full = workfile::join_under(work_dir, path)?;
    let new = std::fs::read(&full).map_err(|source| ActionError::Git {
        op: OP,
        name: path.to_owned(),
        source: Box::new(source),
    })?;
    let mut ordered = stamps.to_vec();
    ordered.sort_by_key(|stamp| (stamp.old_start, stamp.old_lines));
    let spliced =
        workfile::splice(&old, &new, &ordered).map_err(|_| ActionError::HunkMismatch {
            path: path.to_owned(),
        })?;
    let oid = write_blob(repo, path, &spliced)?;
    // The partially committed file keeps HEAD's kind: the exec bit only changes when the
    // whole file is committed.
    Ok(PathEdit::Put {
        oid,
        kind: entry.mode.into(),
    })
}

/// The current hunks of `path`'s worktree diff against the blob `old`, computed by the same
/// code that built the UI's hunks. A binary or over-limit file has no hunks to select, so it
/// reports a mismatch.
pub(super) fn recompute_hunks(
    repo_path: &Path,
    path: &str,
    old: ObjectId,
) -> ActionResult<Vec<rc_git_core::DiffHunk>> {
    let mismatch = || ActionError::HunkMismatch {
        path: path.to_owned(),
    };
    let core = rc_git_core::Repository::open(repo_path).map_err(|source| ActionError::Git {
        op: OP,
        name: path.to_owned(),
        source: Box::new(source),
    })?;
    let old = rc_git_core::Oid::from_bytes(old.as_slice()).ok_or_else(mismatch)?;
    let diff = core
        .diff_worktree_file(
            Some(old),
            path,
            rc_git_core::DEFAULT_BLOB_LIMIT,
            rc_git_core::DEFAULT_DIFF_CONTEXT,
        )
        .map_err(|source| ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: Box::new(source),
        })?;
    match diff {
        rc_git_core::BlobDiff::Text { hunks } => Ok(hunks),
        rc_git_core::BlobDiff::Binary { .. } | rc_git_core::BlobDiff::TooLarge { .. } => {
            Err(mismatch())
        }
    }
}

/// Whether `stamp` names one of `hunks` exactly.
pub(super) fn stamp_matches(stamp: HunkStamp, hunks: &[rc_git_core::DiffHunk]) -> bool {
    hunks.iter().any(|hunk| {
        (
            hunk.old_start,
            hunk.old_lines,
            hunk.new_start,
            hunk.new_lines,
        ) == (
            stamp.old_start,
            stamp.old_lines,
            stamp.new_start,
            stamp.new_lines,
        )
    })
}

/// Every stamp must match a hunk of a fresh diff, or the file moved under the selection.
fn verify_stamps(
    repo_path: &Path,
    path: &str,
    old: ObjectId,
    stamps: &[HunkStamp],
) -> ActionResult<()> {
    let hunks = recompute_hunks(repo_path, path, old)?;
    for stamp in stamps {
        if !stamp_matches(*stamp, &hunks) {
            return Err(ActionError::HunkMismatch {
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

pub(super) fn tree_entry_at(
    repo: &gix::Repository,
    base_tree: Option<ObjectId>,
    path: &str,
) -> ActionResult<Option<gix::objs::tree::Entry>> {
    let Some(base_tree) = base_tree else {
        return Ok(None);
    };
    let tree = repo
        .find_tree(base_tree)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: base_tree.to_string(),
            source: Box::new(source),
        })?;
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::as_bytes);
    let found = tree
        .lookup_entry(segments)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: Box::new(source),
        })?;
    Ok(found.map(|entry| gix::objs::tree::Entry {
        mode: entry.mode(),
        filename: entry.filename().to_owned(),
        oid: entry.object_id(),
    }))
}

pub(super) fn blob_data(
    repo: &gix::Repository,
    path: &str,
    oid: ObjectId,
) -> ActionResult<Vec<u8>> {
    let mut blob = repo.find_blob(oid).map_err(|source| ActionError::Git {
        op: OP,
        name: path.to_owned(),
        source: Box::new(source),
    })?;
    Ok(blob.take_data())
}

fn write_blob(repo: &gix::Repository, path: &str, data: &[u8]) -> ActionResult<ObjectId> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    repo.write_blob(data)
        .map(gix::Id::detach)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: path.to_owned(),
            source: Box::new(source),
        })
}

pub(super) fn write_object(
    repo: &gix::Repository,
    object: &impl gix::objs::WriteTo,
) -> ActionResult<ObjectId> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    repo.write_object(object)
        .map(gix::Id::detach)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: "object".to_owned(),
            source: Box::new(source),
        })
}

/// Upserts the committed paths into `.git/index` — new blob id, mode, and a fresh stat — and
/// removes deleted ones, leaving every other entry untouched, so terminal `git status` shows
/// the committed paths as clean instead of staged reverse-modifications. The stale
/// tree-cache extension is dropped rather than rewritten; git rebuilds it on demand.
fn update_index(
    repo: &gix::Repository,
    work_dir: &Path,
    changes: &[(String, Option<(ObjectId, EntryKind)>)],
) -> ActionResult<()> {
    let index_path = repo.index_path();
    let mut file = gix::index::File::at_or_default(
        index_path,
        repo.object_hash(),
        false,
        gix::index::decode::Options::default(),
    )
    .map_err(|source| ActionError::Git {
        op: OP,
        name: "index".to_owned(),
        source: Box::new(source),
    })?;
    for (path, change) in changes {
        file.remove_entries(|_, entry_path, _| entry_path == path.as_bytes().as_bstr());
        if let Some((oid, kind)) = change {
            let stat = workfile::join_under(work_dir, path)
                .ok()
                .and_then(|full| gix::index::fs::Metadata::from_path_no_follow(&full).ok())
                .and_then(|meta| gix::index::entry::Stat::from_fs(&meta).ok())
                .unwrap_or_default();
            file.dangerously_push_entry(
                stat,
                *oid,
                gix::index::entry::Flags::empty(),
                index_mode(*kind),
                path.as_bytes().as_bstr(),
            );
        }
    }
    file.sort_entries();
    file.remove_tree();
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    file.write(gix::index::write::Options::default())
        .map_err(|source| ActionError::Git {
            op: OP,
            name: "index".to_owned(),
            source: Box::new(source),
        })?;
    Ok(())
}

fn index_mode(kind: EntryKind) -> gix::index::entry::Mode {
    match kind {
        EntryKind::Link => gix::index::entry::Mode::SYMLINK,
        EntryKind::BlobExecutable => gix::index::entry::Mode::FILE_EXECUTABLE,
        EntryKind::Blob | EntryKind::Tree | EntryKind::Commit => gix::index::entry::Mode::FILE,
    }
}

fn git_error(message: &'static str) -> ActionError {
    ActionError::Git {
        op: OP,
        name: "HEAD".to_owned(),
        source: message.into(),
    }
}

fn git_error_for(path: &str, message: &'static str) -> ActionError {
    ActionError::Git {
        op: OP,
        name: path.to_owned(),
        source: message.into(),
    }
}

/// A commit message with surrounding whitespace trimmed and one trailing newline; an empty
/// message is refused.
fn normalized_message(message: &str) -> ActionResult<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(git_error("the commit message is empty"));
    }
    Ok(format!("{trimmed}\n"))
}
