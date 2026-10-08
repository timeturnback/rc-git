//! Commit selected working-tree changes: blobs from disk, a rewritten tree, a commit
//! object, the branch ref through [`apply_edit`](super::apply_edit), and an index update so
//! terminal `git status` agrees afterwards. Raw bytes throughout: gitattributes filters and
//! eol conversion are not applied, matching how the core compares and diffs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gix::ObjectId;
use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::Stat;
use gix::objs::tree::EntryKind;
use gix::refs::Target;
use gix::refs::transaction::PreviousValue;

use super::treebuild::{self, Node, PathEdit};
use super::{
    ActionError, ActionResult, Author, FileSelection, HunkStamp, Identity, confirm, git_err, open,
    plan_commit, treeapply, workfile,
};
use crate::Policy;

/// What a completed commit produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The new commit.
    pub oid: rc_git_core::Oid,
    /// Short name of the branch that advanced.
    pub branch: String,
    /// Whether `.git/index` was updated to the new commit. `false` when the branch moved but the
    /// index write failed afterwards: the commit stands, and terminal `git status` shows the
    /// committed paths as staged reverse changes until `git reset -q -- <paths>` runs.
    pub index_refreshed: bool,
}

const OP: &str = "commit";

/// The repository's own `user.name`/`user.email`, as a commit would record them without an
/// [`Author::explicit`] identity: `None` when its config sets either one empty or not at all.
/// Read fresh from disk, so an edit to `.git/config` is seen on the next call.
///
/// # Errors
/// [`ActionError::Git`] when the repository cannot be opened.
pub fn configured_identity(repo_path: &Path) -> ActionResult<Option<Identity>> {
    let repo = open(repo_path, "read commit identity", "config")?;
    Ok(workfile::repository_identity(&repo))
}

/// Commits `selections` to the checked-out branch with `message`. `expected_head` is the
/// commit the change list was built against — `None` for an unborn branch — and the commit
/// is refused when `HEAD` moved away from it. `author` decides who is recorded, as
/// [`Author`] describes; without any identity the commit is refused. Whole-file selections
/// commit the bytes on disk; hunk selections are re-validated against a fresh diff and
/// spliced onto the tree side.
///
/// # Errors
/// [`ActionError::DetachedHead`], [`ActionError::StaleRepository`],
/// [`ActionError::MissingIdentity`], [`ActionError::HunkMismatch`] as described,
/// [`ActionError::NoHunksSelected`] for a hunk selection with no hunks, and
/// [`ActionError::Git`] when an object cannot be read or written, or a selected path is one
/// git refuses (`.git`, `.`, `..`, an empty segment, a symlinked parent) — checked for every
/// selection before anything is read or written.
pub fn commit(
    repo_path: &Path,
    policy: &Policy,
    message: &str,
    author: Author,
    expected_head: Option<rc_git_core::Oid>,
    selections: &[FileSelection],
    input: Option<&str>,
) -> ActionResult<Committed> {
    let repo = open(repo_path, OP, "HEAD")?;
    let _proof = confirm(plan_commit(policy, selections.len()), OP, "HEAD", input)?;
    let message = normalized_message(message)?;
    let (branch_ref, head_commit) = checked_out_branch(&repo, OP)?;
    let current = head_commit
        .map(|id| treeapply::core_oid(id, OP))
        .transpose()?;
    if current != expected_head {
        return Err(ActionError::StaleRepository { op: OP });
    }
    if selections.is_empty() {
        return Err(ActionError::git(OP, "HEAD", "nothing selected to commit"));
    }
    let index = LockedIndex::acquire(&repo, OP)?;
    let work_dir = work_dir(&repo, OP)?;
    let selected = selections.iter().map(|selection| match selection {
        FileSelection::Whole { path } | FileSelection::Hunks { path, .. } => path.as_str(),
    });
    workfile::check_paths(&work_dir, selected.clone(), OP)?;
    let identity = workfile::resolve_identity(&repo, author)?;
    let base_tree = head_commit.map(|id| tree_of(&repo, OP, id)).transpose()?;
    for path in selected {
        refuse_case_variant_path(&repo, &work_dir, base_tree, path, OP)?;
    }

    let (edits, index_changes) =
        selection_edits(&repo, repo_path, &work_dir, base_tree, selections)?;

    let new_tree = match treebuild::rewrite(&repo, base_tree, &edits)? {
        Some(oid) => oid,
        None => write_object(&repo, OP, &gix::objs::Tree::empty())?,
    };
    if Some(new_tree) == base_tree {
        return Err(ActionError::git(
            OP,
            "HEAD",
            "the selection matches HEAD; nothing to commit",
        ));
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
    let commit_oid = write_object(&repo, OP, &commit_object)?;
    let oid = treeapply::core_oid(commit_oid, OP)?;

    let expected = match head_commit {
        Some(id) => PreviousValue::MustExistAndMatch(Target::Object(id)),
        None => PreviousValue::MustNotExist,
    };
    let edit = super::update_edit(
        &branch_ref,
        expected,
        Target::Object(commit_oid),
        "rc-git: commit",
        OP,
    )?;
    super::apply_edit(&repo, edit, OP, &branch_ref)?;
    // The branch has moved, so the commit stands whatever happens to the index: an error here
    // would read as a failed commit and a retry would be refused as stale. The flag reports it.
    let index_refreshed = update_index(index, OP, &index_changes).is_ok();

    Ok(Committed {
        oid,
        branch: branch_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(&branch_ref)
            .to_owned(),
        index_refreshed,
    })
}

/// A path and the blob, kind and stat the index records for it; `None` removes its entry.
pub(in crate::actions) type IndexChange = (String, Option<(ObjectId, EntryKind, Stat)>);

/// The tree edits for every selection, and the index changes that make terminal
/// `git status` agree once the commit lands.
fn selection_edits(
    repo: &gix::Repository,
    repo_path: &Path,
    work_dir: &Path,
    base_tree: Option<ObjectId>,
    selections: &[FileSelection],
) -> ActionResult<(BTreeMap<BString, Node>, Vec<IndexChange>)> {
    let mut edits: BTreeMap<BString, Node> = BTreeMap::new();
    let mut index_changes: Vec<IndexChange> = Vec::new();
    for selection in selections {
        let (path, edit, stat) = match selection {
            FileSelection::Whole { path } => {
                // Statted before the read: an edit landing after it changes the stat, so git
                // rehashes the file instead of trusting it to match the committed blob.
                let stat = disk_stat(work_dir, path, OP);
                let edit = whole_file_edit(repo, OP, work_dir, base_tree, path)?;
                (path, edit, stat)
            }
            FileSelection::Hunks { path, hunks } => {
                let edit = spliced_edit(repo, repo_path, work_dir, base_tree, path, hunks)?;
                // Disk still holds the unselected hunks, so no stat of it proves the spliced
                // blob; the zeroed stat `git apply --cached` writes makes every reader rehash.
                (path, edit, Stat::default())
            }
        };
        index_changes.push((
            path.clone(),
            match &edit {
                PathEdit::Put { oid, kind } => Some((*oid, *kind, stat)),
                PathEdit::Remove => None,
            },
        ));
        treebuild::insert(&mut edits, path, edit)?;
    }
    Ok((edits, index_changes))
}

/// The full ref name `HEAD` points at and the commit it resolves to; `None` for an unborn
/// branch. A detached `HEAD` is refused: v1 commits to a branch or not at all. Errors name
/// `op`.
pub(super) fn checked_out_branch(
    repo: &gix::Repository,
    op: &'static str,
) -> ActionResult<(String, Option<ObjectId>)> {
    let head_name = repo
        .head_name()
        .map_err(git_err(op, "HEAD"))?
        .ok_or(ActionError::DetachedHead { op })?;
    let commit = repo.head_id().ok().map(gix::Id::detach);
    Ok((head_name.as_bstr().to_string(), commit))
}

pub(super) fn work_dir(repo: &gix::Repository, op: &'static str) -> ActionResult<PathBuf> {
    repo.workdir().map(Path::to_path_buf).ok_or_else(|| {
        ActionError::git(op, "HEAD", "the repository is bare and has no working tree")
    })
}

pub(super) fn tree_of(
    repo: &gix::Repository,
    op: &'static str,
    commit: ObjectId,
) -> ActionResult<ObjectId> {
    let commit = repo
        .find_commit(commit)
        .map_err(|source| ActionError::git(op, commit.to_string(), source))?;
    commit
        .tree_id()
        .map(gix::Id::detach)
        .map_err(|source| ActionError::git(op, commit.id.to_string(), source))
}

/// Under `core.ignoreCase`, refuses a selected path any segment of which matches an entry on
/// disk or in `base_tree` only by case: the volume would read `F` through `f`, and the commit
/// would store the bytes under a second name beside the old entry. Each segment is compared
/// with its parent directory's listing and the base tree's entries, so the cost is one
/// listing and one tree per segment.
///
/// # Errors
/// [`ActionError::Git`] naming `op`, the path, and both spellings, or when a directory or
/// tree cannot be read.
pub(super) fn refuse_case_variant_path(
    repo: &gix::Repository,
    work_dir: &Path,
    base_tree: Option<ObjectId>,
    path: &str,
    op: &'static str,
) -> ActionResult<()> {
    if !super::ignores_case(repo) {
        return Ok(());
    }
    let mut dir = work_dir.to_path_buf();
    let mut tree = base_tree;
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        if let Some(found) = disk_case_variant(&dir, segment, path, op)? {
            return Err(case_variant_error(op, path, segment, &found, "on disk"));
        }
        let (subtree, variant) = tree_case_variant(repo, tree, segment, path, op)?;
        if let Some(found) = variant {
            return Err(case_variant_error(op, path, segment, &found, "in HEAD"));
        }
        tree = subtree;
        dir.push(segment);
    }
    Ok(())
}

fn case_variant_error(
    op: &'static str,
    path: &str,
    segment: &str,
    found: &str,
    place: &str,
) -> ActionError {
    ActionError::git(
        op,
        path,
        format!(
            "'{segment}' is spelled '{found}' {place}, and this repository ignores case \
             (core.ignoreCase); select the path as '{found}' or use git directly"
        ),
    )
}

/// The name in `parent` that equals `segment` only by case, when no entry has exactly that
/// name; `None` when one does, when none matches, or when `parent` is not a directory.
fn disk_case_variant(
    parent: &Path,
    segment: &str,
    path: &str,
    op: &'static str,
) -> ActionResult<Option<String>> {
    let read_error = git_err(op, path);
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        // A file where `parent` was (a directory replaced by a file) has no entries either.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(read_error(error)),
    };
    let folded = segment.to_lowercase();
    let mut variant = None;
    for entry in entries {
        let name = entry.map_err(read_error)?.file_name();
        let name = name.to_string_lossy();
        if name == segment {
            return Ok(None);
        }
        if name.to_lowercase() == folded {
            variant = Some(name.into_owned());
        }
    }
    Ok(variant)
}

/// Looks `segment` up in `tree`: the subtree to descend into when the entry exists exactly
/// and is a directory, and the entry's name when it matches only by case.
fn tree_case_variant(
    repo: &gix::Repository,
    tree: Option<ObjectId>,
    segment: &str,
    path: &str,
    op: &'static str,
) -> ActionResult<(Option<ObjectId>, Option<String>)> {
    let Some(tree) = tree else {
        return Ok((None, None));
    };
    let tree = repo.find_tree(tree).map_err(git_err(op, path))?;
    let decoded = tree.decode().map_err(git_err(op, path))?;
    let folded = segment.to_lowercase();
    let mut variant = None;
    for entry in &decoded.entries {
        if entry.filename == segment.as_bytes() {
            let subtree = entry.mode.is_tree().then(|| entry.oid.to_owned());
            return Ok((subtree, None));
        }
        if entry.filename.to_str_lossy().to_lowercase() == folded {
            variant = Some(entry.filename.to_string());
        }
    }
    Ok((None, variant))
}

/// The edit for committing `path` exactly as disk has it: an addition or replacement from
/// the file's bytes, or a removal when the file is gone. Errors name `op`.
pub(super) fn whole_file_edit(
    repo: &gix::Repository,
    op: &'static str,
    work_dir: &Path,
    base_tree: Option<ObjectId>,
    path: &str,
) -> ActionResult<PathEdit> {
    let (data, kind) = match workfile::disk_entry(work_dir, path, op)? {
        workfile::DiskEntry::Present { data, kind } => (data, kind),
        workfile::DiskEntry::Directory => {
            return Err(ActionError::git(
                op,
                path,
                "is a directory; select files instead",
            ));
        }
        workfile::DiskEntry::Absent => {
            if tree_entry_at(repo, op, base_tree, path)?.is_none() {
                return Err(ActionError::git(
                    op,
                    path,
                    "does not exist on disk or in HEAD",
                ));
            }
            return Ok(PathEdit::Remove);
        }
    };
    if kind == EntryKind::Link {
        workfile::validate_path(op, path, true)?;
    }
    let kind = recorded_file_kind(repo, op, base_tree, path, kind)?;
    let oid = write_blob(repo, op, path, &data)?;
    Ok(PathEdit::Put { oid, kind })
}

/// The kind to record for a file read from disk as `disk`. With `core.fileMode` off the disk's
/// executable bit is not evidence, so a tracked file keeps `base_tree`'s kind and a new one is a
/// plain blob — the mode git itself would record.
fn recorded_file_kind(
    repo: &gix::Repository,
    op: &'static str,
    base_tree: Option<ObjectId>,
    path: &str,
    disk: EntryKind,
) -> ActionResult<EntryKind> {
    if disk == EntryKind::Link {
        return Ok(disk);
    }
    let file_mode = repo
        .filesystem_options()
        .map_err(git_err(op, "core.fileMode"))?
        .executable_bit;
    if file_mode {
        return Ok(disk);
    }
    let tracked = tree_entry_at(repo, op, base_tree, path)?.map(|entry| entry.mode.kind());
    Ok(match tracked {
        Some(kind @ (EntryKind::Blob | EntryKind::BlobExecutable)) => kind,
        Some(_) | None => EntryKind::Blob,
    })
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
        return Err(ActionError::NoHunksSelected {
            op: OP,
            path: path.to_owned(),
        });
    }
    let entry =
        tree_entry_at(repo, OP, base_tree, path)?.ok_or_else(|| ActionError::HunkMismatch {
            path: path.to_owned(),
        })?;
    if !entry.mode.is_blob() {
        return Err(ActionError::HunkMismatch {
            path: path.to_owned(),
        });
    }
    verified_hunks(repo_path, OP, path, entry.oid, stamps)?;

    let old = blob_data(repo, OP, path, entry.oid)?;
    let full = workfile::join_under(work_dir, path, OP)?;
    let new = std::fs::read(&full).map_err(git_err(OP, path))?;
    let mut ordered = stamps.to_vec();
    ordered.sort_by_key(|stamp| (stamp.old_start, stamp.old_lines));
    let spliced =
        workfile::splice(&old, &new, &ordered).map_err(|_| ActionError::HunkMismatch {
            path: path.to_owned(),
        })?;
    let oid = write_blob(repo, OP, path, &spliced)?;
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
fn recompute_hunks(
    repo_path: &Path,
    op: &'static str,
    path: &str,
    old: ObjectId,
) -> ActionResult<Vec<rc_git_core::DiffHunk>> {
    let mismatch = || ActionError::HunkMismatch {
        path: path.to_owned(),
    };
    let core = rc_git_core::Repository::open(repo_path).map_err(git_err(op, path))?;
    let old = rc_git_core::Oid::from_bytes(old.as_slice()).ok_or_else(mismatch)?;
    let diff = core
        .diff_worktree_file(
            Some(old),
            path,
            rc_git_core::DEFAULT_BLOB_LIMIT,
            rc_git_core::DEFAULT_DIFF_CONTEXT,
        )
        .map_err(git_err(op, path))?;
    match diff {
        rc_git_core::BlobDiff::Text { hunks } => Ok(hunks),
        rc_git_core::BlobDiff::Binary { .. } | rc_git_core::BlobDiff::TooLarge { .. } => {
            Err(mismatch())
        }
    }
}

/// The current hunks of `path`'s worktree diff against the blob `old`, after checking that
/// every stamp names one of them exactly; otherwise the file moved under the selection.
///
/// # Errors
/// [`ActionError::HunkMismatch`] when a stamp matches no current hunk or the file has no text
/// diff, [`ActionError::Git`] when the diff cannot be computed.
pub(super) fn verified_hunks(
    repo_path: &Path,
    op: &'static str,
    path: &str,
    old: ObjectId,
    stamps: &[HunkStamp],
) -> ActionResult<Vec<rc_git_core::DiffHunk>> {
    let hunks = recompute_hunks(repo_path, op, path, old)?;
    for stamp in stamps {
        if !hunks.iter().any(|hunk| HunkStamp::from(hunk) == *stamp) {
            return Err(ActionError::HunkMismatch {
                path: path.to_owned(),
            });
        }
    }
    Ok(hunks)
}

pub(super) fn tree_entry_at(
    repo: &gix::Repository,
    op: &'static str,
    base_tree: Option<ObjectId>,
    path: &str,
) -> ActionResult<Option<gix::objs::tree::Entry>> {
    let Some(base_tree) = base_tree else {
        return Ok(None);
    };
    let mut tree = repo
        .find_tree(base_tree)
        .map_err(|source| ActionError::git(op, base_tree.to_string(), source))?;
    let mut segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .peekable();
    // Walked by hand: a gitlink's oid names a commit in another repository, so a path through
    // a submodule (or a file) reads as absent instead of failing to find that commit.
    while let Some(segment) = segments.next() {
        let Some(entry) = tree.find_entry(segment.as_bytes()) else {
            return Ok(None);
        };
        let (mode, oid) = (entry.mode(), entry.object_id());
        if segments.peek().is_none() {
            let filename = entry.filename().to_owned();
            return Ok(Some(gix::objs::tree::Entry {
                mode,
                filename,
                oid,
            }));
        }
        if !mode.is_tree() {
            return Ok(None);
        }
        tree = repo.find_tree(oid).map_err(git_err(op, path))?;
    }
    Ok(None)
}

pub(super) fn blob_data(
    repo: &gix::Repository,
    op: &'static str,
    path: &str,
    oid: ObjectId,
) -> ActionResult<Vec<u8>> {
    let mut blob = repo.find_blob(oid).map_err(git_err(op, path))?;
    Ok(blob.take_data())
}

fn write_blob(
    repo: &gix::Repository,
    op: &'static str,
    path: &str,
    data: &[u8],
) -> ActionResult<ObjectId> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    repo.write_blob(data)
        .map(gix::Id::detach)
        .map_err(git_err(op, path))
}

pub(super) fn write_object(
    repo: &gix::Repository,
    op: &'static str,
    object: &impl gix::objs::WriteTo,
) -> ActionResult<ObjectId> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    repo.write_object(object)
        .map(gix::Id::detach)
        .map_err(git_err(op, "object"))
}

/// The lstat of `path` below `work_dir` as the index records it. A path that cannot be
/// statted gets the zeroed stat, which only makes git rehash the file, never trust it.
pub(in crate::actions) fn disk_stat(work_dir: &Path, path: &str, op: &'static str) -> Stat {
    workfile::join_under(work_dir, path, op)
        .ok()
        .and_then(|full| gix::index::fs::Metadata::from_path_no_follow(&full).ok())
        .and_then(|meta| Stat::from_fs(&meta).ok())
        .unwrap_or_default()
}

/// `.git/index` read while holding `index.lock`, as git does: nothing else can commit an index
/// between this read and [`update_index`], so a concurrent `git add` is never overwritten with
/// stale entries. Dropping it without [`update_index`] removes the lock and leaves the index as
/// it was.
pub(in crate::actions) struct LockedIndex {
    lock: gix::lock::File,
    file: gix::index::File,
}

impl LockedIndex {
    /// Takes `index.lock`, failing at once when another process holds it, then reads the index.
    /// Called before the action writes anything, so a held lock refuses the whole action.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `index` when another git process holds the lock,
    /// or when the lock cannot be created or the index cannot be read.
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    pub(in crate::actions) fn acquire(
        repo: &gix::Repository,
        op: &'static str,
    ) -> ActionResult<Self> {
        let index_path = repo.index_path();
        let lock = gix::lock::File::acquire_to_update_resource(
            &index_path,
            gix::lock::acquire::Fail::Immediately,
            None,
        )
        .map_err(|source| match source {
            gix::lock::acquire::Error::PermanentlyLocked { .. } => ActionError::git(
                op,
                "index",
                "the index is locked by another git process; retry",
            ),
            gix::lock::acquire::Error::Io(source) => ActionError::git(op, "index", source),
        })?;
        let file = gix::index::File::at_or_default(
            index_path,
            repo.object_hash(),
            false,
            gix::index::decode::Options::default(),
        )
        .map_err(git_err(op, "index"))?;
        Ok(Self { lock, file })
    }

    /// The index as read under the lock.
    pub(in crate::actions) fn file(&self) -> &gix::index::File {
        &self.file
    }
}

/// Every strict ancestor directory of a path `changes` upserts. An index entry with one of these
/// names is a file the new tree replaced with a directory, as git's add-with-replace drops it.
fn file_ancestors_of_upserts<'a>(
    changes: &BTreeMap<&'a BStr, &Option<(ObjectId, EntryKind, Stat)>>,
) -> std::collections::BTreeSet<&'a BStr> {
    changes
        .iter()
        .filter(|(_, change)| change.is_some())
        .flat_map(|(path, _)| strict_ancestors(path))
        .collect()
}

/// `a`, `a/b` for the path `a/b/c`: each proper prefix ending before a `/`.
fn strict_ancestors(path: &BStr) -> impl Iterator<Item = &BStr> {
    path.iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'/')
        .map(move |(at, _)| path[..at].as_bstr())
}

/// Upserts the committed paths into `.git/index` — new blob id, mode, and the stat the caller
/// took when it read or wrote the file — and removes deleted ones, leaving every other entry
/// untouched. A changed path also drops the entries below it (a directory it replaced or that
/// was removed as a whole) and an upserted one drops entries named like its ancestors (a file
/// a directory replaced), as the rewritten tree did. Terminal `git status` then shows the
/// committed paths as clean instead of staged reverse-modifications. The stale tree-cache extension is dropped rather than rewritten; git
/// rebuilds it on demand. Written through the lock `index` holds, then committed over the index.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and `index` when the lock file cannot be written or
/// renamed over the index.
#[expect(
    clippy::disallowed_methods,
    reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
)]
pub(in crate::actions) fn update_index(
    index: LockedIndex,
    op: &'static str,
    changes: &[IndexChange],
) -> ActionResult<()> {
    let LockedIndex { lock, mut file } = index;
    // One pass over the index for every changed path: a removal per path rescans the whole
    // index, which costs seconds when a switch touches thousands of paths in a large one. The
    // map keeps the last change per path, as applying them in order would.
    let by_path: BTreeMap<&BStr, &Option<(ObjectId, EntryKind, Stat)>> = changes
        .iter()
        .map(|(path, change)| (path.as_bytes().as_bstr(), change))
        .collect();
    let replaced_files = file_ancestors_of_upserts(&by_path);
    file.remove_entries(|_, entry_path, _| {
        by_path.contains_key(entry_path)
            || replaced_files.contains(entry_path)
            || strict_ancestors(entry_path).any(|ancestor| by_path.contains_key(ancestor))
    });
    for (path, change) in &by_path {
        if let Some((oid, kind, stat)) = change {
            file.dangerously_push_entry(
                *stat,
                *oid,
                gix::index::entry::Flags::empty(),
                index_mode(*kind),
                path,
            );
        }
    }
    file.sort_entries();
    file.remove_tree();
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, lock);
    file.write_to(&mut out, gix::index::write::Options::default())
        .map_err(git_err(op, "index"))?;
    let lock = out
        .into_inner()
        .map_err(|source| ActionError::git(op, "index", source.into_error()))?;
    lock.commit()
        .map_err(|source| ActionError::git(op, "index", source.error))?;
    Ok(())
}

fn index_mode(kind: EntryKind) -> gix::index::entry::Mode {
    match kind {
        EntryKind::Link => gix::index::entry::Mode::SYMLINK,
        EntryKind::BlobExecutable => gix::index::entry::Mode::FILE_EXECUTABLE,
        EntryKind::Blob | EntryKind::Tree | EntryKind::Commit => gix::index::entry::Mode::FILE,
    }
}

/// A commit message with surrounding whitespace trimmed and one trailing newline; an empty
/// message is refused.
fn normalized_message(message: &str) -> ActionResult<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(ActionError::git(OP, "HEAD", "the commit message is empty"));
    }
    Ok(format!("{trimmed}\n"))
}
