//! Every write rc-git makes, as methods on a [`Writer`]: ref edits, objects, the index, the
//! working tree, and the reflogs a ref transaction does not keep. A `Writer` can only be built
//! from a [`Confirmation`], which only the policy mints, so an entry point that skips the
//! policy has nothing to write with and does not compile. Each method carries its own
//! `#[expect(clippy::disallowed_methods)]` opt-out; nothing else in the crate writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::index::entry::Stat;
use gix::objs::tree::EntryKind;
use gix::refs::Target;
use gix::refs::transaction::{Change, PreviousValue, RefEdit};

use super::commit::{IndexChange, index_mode};
use super::{ActionError, ActionResult, REFLOG_EMAIL, REFLOG_NAME, git_err, update_edit, workfile};
use crate::Confirmation;

/// How long [`Writer::lock_stash_log`] waits for `logs/refs/stash.lock`: gix's default ref-file
/// lock timeout (`core.filesRefLockTimeout`), so a lock another writer holds briefly does not
/// fail the drop.
const LOG_LOCK_WAIT: Duration = Duration::from_millis(100);

/// How many temporary names [`write_temp`] tries before it gives up.
const TEMP_ATTEMPTS: u32 = 16;

/// The write half of one action on `repo`, held only while the action's [`Confirmation`] is.
pub(super) struct Writer<'a> {
    repo: &'a gix::Repository,
    proof: &'a Confirmation,
}

/// `.git/index` read while holding `index.lock`, as git does: nothing else can commit an index
/// between this read and [`Writer::update_index`], so a concurrent `git add` is never
/// overwritten with stale entries. Dropping it without [`Writer::update_index`] removes the lock
/// and leaves the index as it was.
pub(super) struct LockedIndex {
    lock: gix::lock::File,
    file: gix::index::File,
}

impl LockedIndex {
    /// The index as read under the lock.
    pub(super) fn file(&self) -> &gix::index::File {
        &self.file
    }
}

impl<'a> Writer<'a> {
    /// A writer for `repo`, allowed by `proof`.
    pub(super) fn new(repo: &'a gix::Repository, proof: &'a Confirmation) -> Self {
        Self { repo, proof }
    }

    /// The repository this writer writes to, for the reads an action makes between writes.
    pub(super) fn repo(&self) -> &'a gix::Repository {
        self.repo
    }

    /// Whether the answered challenge asked for a backup ref before the action runs.
    pub(super) fn backup_required(&self) -> bool {
        self.proof.backup_ref()
    }

    /// Applies one ref edit as the synthetic `rc-git` committer, so reflog entries never depend
    /// on user configuration the isolated open refuses to read.
    ///
    /// # Errors
    /// As [`Writer::apply_edit_as`].
    pub(super) fn apply_edit(
        &self,
        edit: RefEdit,
        op: &'static str,
        name: &str,
    ) -> ActionResult<()> {
        self.apply_edit_as(edit, op, name, (REFLOG_NAME, REFLOG_EMAIL))
    }

    /// Applies one ref edit with an explicit reflog `(name, email)` committer. The stash uses
    /// the user's own identity here because `git stash list` shows the reflog's author;
    /// everything else goes through [`Writer::apply_edit`].
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `name` when the reflog message spans more than one
    /// line or the transaction fails.
    pub(super) fn apply_edit_as(
        &self,
        edit: RefEdit,
        op: &'static str,
        name: &str,
        (committer_name, committer_email): (&str, &str),
    ) -> ActionResult<()> {
        if let Change::Update { log, .. } = &edit.change
            && log.message.contains(&b'\n')
        {
            return Err(ActionError::git(
                op,
                name,
                "the reflog message spans more than one line",
            ));
        }
        let time = gix::date::Time::now_local_or_utc()
            .format(gix::date::time::Format::Raw)
            .unwrap_or_else(|_| "0 +0000".to_owned());
        let committer = gix::actor::SignatureRef {
            name: committer_name.into(),
            email: committer_email.into(),
            time: &time,
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        self.repo
            .edit_references_as(Some(edit), Some(committer))
            .map_err(git_err(op, name))?;
        Ok(())
    }

    /// Writes `refs/rc-git/backup/<kind>/<name>` pointing at `target`, appending `-2`, `-3`, …
    /// when earlier backups of the same name exist, so nothing is ever overwritten.
    ///
    /// # Errors
    /// [`ActionError::Git`] when no free slot could be written.
    pub(super) fn write_backup_ref(
        &self,
        kind: &str,
        name: &str,
        target: ObjectId,
    ) -> ActionResult<String> {
        const OP: &str = "write backup ref";
        for attempt in 1..=1000_u32 {
            let candidate = if attempt == 1 {
                format!("refs/rc-git/backup/{kind}/{name}")
            } else {
                format!("refs/rc-git/backup/{kind}/{name}-{attempt}")
            };
            let edit = update_edit(
                &candidate,
                PreviousValue::MustNotExist,
                Target::Object(target),
                "rc-git: backup before deletion",
                OP,
            )?;
            match self.apply_edit(edit, OP, &candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) => {
                    if attempt == 1000 {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("the loop always returns")
    }

    /// Writes `data` as a blob read from `path`.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `path` when the object cannot be written.
    pub(super) fn write_blob(
        &self,
        op: &'static str,
        path: &str,
        data: &[u8],
    ) -> ActionResult<ObjectId> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        self.repo
            .write_blob(data)
            .map(gix::Id::detach)
            .map_err(git_err(op, path))
    }

    /// Writes a tree or commit `object`; errors name `op` and `name`.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `name` when the object cannot be written.
    pub(super) fn write_object(
        &self,
        op: &'static str,
        name: &str,
        object: &impl gix::objs::WriteTo,
    ) -> ActionResult<ObjectId> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        self.repo
            .write_object(object)
            .map(gix::Id::detach)
            .map_err(git_err(op, name))
    }

    /// Takes `index.lock`, failing at once when another process holds it, then reads the index.
    /// Called before the action writes anything else, so a held lock refuses the whole action.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `index` when another git process holds the lock,
    /// or when the lock cannot be created or the index cannot be read.
    pub(super) fn lock_index(&self, op: &'static str) -> ActionResult<LockedIndex> {
        let index_path = self.repo.index_path();
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
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
            self.repo.object_hash(),
            false,
            gix::index::decode::Options::default(),
        )
        .map_err(git_err(op, "index"))?;
        Ok(LockedIndex { lock, file })
    }

    /// Upserts the committed paths into `.git/index` — new blob id, mode, and the stat the
    /// caller took when it read or wrote the file — and removes deleted ones, leaving every
    /// other entry untouched. A changed path also drops the entries below it (a directory it
    /// replaced or that was removed as a whole) and an upserted one drops entries named like
    /// its ancestors (a file a directory replaced), as the rewritten tree did. Terminal
    /// `git status` then shows the committed paths as clean instead of staged
    /// reverse-modifications. The stale tree-cache extension is dropped rather than rewritten;
    /// git rebuilds it on demand. Written through the lock `index` holds, then committed over
    /// the index.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `index` when the lock file cannot be written or
    /// renamed over the index.
    #[expect(
        clippy::unused_self,
        reason = "the Writer is the proof the write was authorized; this write needs no repository"
    )]
    pub(super) fn update_index(
        &self,
        index: LockedIndex,
        op: &'static str,
        changes: &[IndexChange],
    ) -> ActionResult<()> {
        let LockedIndex { lock, mut file } = index;
        // One pass over the index for every changed path: a removal per path rescans the whole
        // index, which costs seconds when a switch touches thousands of paths in a large one.
        // The map keeps the last change per path, as applying them in order would.
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
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        file.write_to(&mut out, gix::index::write::Options::default())
            .map_err(git_err(op, "index"))?;
        let lock = out
            .into_inner()
            .map_err(|source| ActionError::git(op, "index", source.into_error()))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        lock.commit()
            .map_err(|source| ActionError::git(op, "index", source.error))?;
        Ok(())
    }

    /// Opens `logs/HEAD` for append before anything is written, so an action that moves the
    /// checked-out commit refuses up front when it could not log the move, instead of failing
    /// after the ref has moved. `None` when the log does not exist: it is appended only when
    /// it already exists, which is what `force_create_reflog: false` means on every ref edit.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `logs/HEAD` when the log exists but cannot be
    /// opened.
    pub(super) fn open_head_reflog(&self, op: &'static str) -> ActionResult<Option<std::fs::File>> {
        let path = self.repo.git_dir().join("logs").join("HEAD");
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        let opened = std::fs::OpenOptions::new().append(true).open(&path);
        match opened {
            Ok(file) => Ok(Some(file)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(git_err(op, "logs/HEAD")(err)),
        }
    }

    /// Appends the line git writes to `logs/HEAD` whenever the checked-out commit moves, to
    /// `file` from [`Writer::open_head_reflog`]. A ref transaction will not write it: a
    /// symbolic update of `HEAD` carries no object id, and a direct update of the branch logs
    /// only the branch. The line is serialized by gix's own reflog writer and carries the same
    /// synthetic `rc-git` committer as every other ref edit.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `logs/HEAD` when the line cannot be written.
    pub(super) fn append_head_reflog(
        &self,
        mut file: std::fs::File,
        (old, new): (Option<ObjectId>, ObjectId),
        message: &str,
        op: &'static str,
    ) -> ActionResult<()> {
        use std::io::Write;
        let line = gix::refs::log::Line {
            previous_oid: old.unwrap_or_else(|| ObjectId::null(self.repo.object_hash())),
            new_oid: new,
            signature: gix::actor::Signature {
                name: REFLOG_NAME.into(),
                email: REFLOG_EMAIL.into(),
                time: gix::date::Time::now_local_or_utc(),
            },
            message: message.into(),
        };
        let mut serialized = Vec::new();
        line.write_to(&mut serialized)
            .map_err(git_err(op, "logs/HEAD"))?;
        file.write_all(&serialized)
            .map_err(git_err(op, "logs/HEAD"))
    }

    /// Takes `logs/refs/stash.lock`, waiting up to [`LOG_LOCK_WAIT`] for another holder.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `refs/stash` when the lock cannot be taken.
    #[expect(
        clippy::unused_self,
        reason = "the Writer is the proof the write was authorized; this write needs no repository"
    )]
    pub(super) fn lock_stash_log(
        &self,
        log_path: &Path,
        op: &'static str,
    ) -> ActionResult<gix::lock::File> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        gix::lock::File::acquire_to_update_resource(
            log_path,
            gix::lock::acquire::Fail::AfterDurationWithBackoff(LOG_LOCK_WAIT),
            None,
        )
        .map_err(|source| ActionError::git(op, "refs/stash", source))
    }

    /// Replaces the stash reflog with exactly `content` by committing the held `lock`.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `refs/stash` when the content cannot be written or
    /// the lock cannot be committed.
    #[expect(
        clippy::unused_self,
        reason = "the Writer is the proof the write was authorized; this write needs no repository"
    )]
    pub(super) fn commit_stash_log(
        &self,
        mut lock: gix::lock::File,
        content: &[u8],
        op: &'static str,
    ) -> ActionResult<()> {
        {
            use std::io::Write;
            lock.write_all(content)
                .map_err(|source| ActionError::git(op, "refs/stash", source))?;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        lock.commit()
            .map_err(|source| ActionError::git(op, "refs/stash", source))?;
        Ok(())
    }

    /// Writes `data` to `relative` below `work_dir` as `kind` describes: a symlink's blob holds
    /// its target, and the executable bit follows the mode. The entry is built under a sibling
    /// temporary name and renamed over the path, so a failed write leaves whatever was there
    /// untouched, and the rename replaces a stale symlink without following it. An empty
    /// directory at the path is removed first. Errors name `op`, the action the write is for.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `relative` for an invalid path, a tree or submodule
    /// `kind`, or a failed write.
    #[expect(
        clippy::unused_self,
        reason = "the Writer is the proof the write was authorized; this write needs no repository"
    )]
    pub(super) fn restore_file(
        &self,
        op: &'static str,
        work_dir: &Path,
        relative: &str,
        (data, kind): (&[u8], EntryKind),
    ) -> ActionResult<()> {
        workfile::validate_path(op, relative, kind == EntryKind::Link)?;
        let full = workfile::join_under(work_dir, relative, op)?;
        if matches!(kind, EntryKind::Tree | EntryKind::Commit) {
            return Err(ActionError::git(
                op,
                relative,
                "only files and symlinks can be restored",
            ));
        }
        let Some(parent) = full.parent() else {
            return Err(ActionError::git(op, relative, "has no parent directory"));
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        {
            std::fs::create_dir_all(parent).map_err(git_err(op, relative))?;
            match std::fs::symlink_metadata(&full) {
                Ok(meta) if meta.is_dir() => {
                    std::fs::remove_dir(&full).map_err(git_err(op, relative))?;
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(ActionError::git(op, relative, error)),
            }
            let temp = write_temp(parent, data, kind).map_err(git_err(op, relative))?;
            if let Err(error) = std::fs::rename(&temp, &full) {
                discard_temp(&temp);
                return Err(ActionError::git(op, relative, error));
            }
        }
        Ok(())
    }

    /// Deletes `relative` below `work_dir`, then removes each parent directory the deletion
    /// left empty, as git does; a file already gone is not an error, so a retried action lands
    /// in the state it retries from. Errors name `op`, the action the delete is for.
    ///
    /// # Errors
    /// [`ActionError::Git`] naming `op` and `relative` for an invalid path or a failed delete.
    #[expect(
        clippy::unused_self,
        reason = "the Writer is the proof the write was authorized; this write needs no repository"
    )]
    pub(super) fn delete_file(
        &self,
        op: &'static str,
        work_dir: &Path,
        relative: &str,
    ) -> ActionResult<()> {
        let full = workfile::join_under(work_dir, relative, op)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ActionError::git(op, relative, error)),
        }
        prune_empty_parents(work_dir, relative);
        Ok(())
    }
}

/// Every strict ancestor directory of a path `changes` upserts. An index entry with one of
/// these names is a file the new tree replaced with a directory, as git's add-with-replace
/// drops it.
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

/// Creates a new entry in `dir` under a name nothing else uses, holding `data` as `kind`
/// describes, and returns its path. The name stays short whatever the final name's length,
/// so it never exceeds the volume's name limit. On failure nothing is left behind.
fn write_temp(dir: &Path, data: &[u8], kind: EntryKind) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..TEMP_ATTEMPTS {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".rc-git-{}-{serial}.tmp", std::process::id()));
        match create_entry(&temp, data, kind) {
            Ok(()) => return Ok(temp),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("no free temporary name in '{}'", dir.display()),
    ))
}

/// Creates `path`, which must not exist yet, as a symlink to `data` or a file holding it.
/// A file left half-written by a failure is removed before the error returns.
fn create_entry(path: &Path, data: &[u8], kind: EntryKind) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
    )]
    {
        if kind == EntryKind::Link {
            use std::os::unix::ffi::OsStrExt;
            return std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(data), path);
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let written = file.write_all(data).and_then(|()| {
            if kind == EntryKind::BlobExecutable {
                file.set_permissions(std::fs::Permissions::from_mode(0o755))
            } else {
                Ok(())
            }
        });
        if written.is_err() {
            discard_temp(path);
        }
        written
    }
}

/// Removes a temporary entry after a failed write. The write's own error is what the caller
/// reports, so a cleanup failure is dropped: the user's path is untouched either way, and a
/// leftover shows up as an untracked `.rc-git-*.tmp` file rather than losing anything.
fn discard_temp(path: &Path) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
    )]
    // Benign: see above; the caller is already returning the error that matters.
    let _ = std::fs::remove_file(path);
}

/// Removes the parent directories of `relative`, deepest first and never `work_dir` itself,
/// stopping at the first one that cannot be removed. Like git's `schedule_dir_for_removal`,
/// a failure here means "still in use" (not empty, or already gone), never an error: the
/// file the caller deleted is gone either way.
fn prune_empty_parents(work_dir: &Path, relative: &str) {
    for (end, _) in relative.rmatch_indices('/') {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's Writer is the one sanctioned write path; see docs/safety.md"
        )]
        let removed = std::fs::remove_dir(work_dir.join(&relative[..end]));
        if removed.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Policy};

    /// The proof an allowed action gets, for tests that call a write primitive directly.
    fn proof() -> Confirmation {
        Policy::default().authorize(&Action::Fetch, None).unwrap()
    }

    /// Restores through a [`Writer`] on the fixture's repository, as an action would.
    fn restore(
        fx: &rc_git_testkit::Fixture,
        relative: &str,
        data: &[u8],
        kind: EntryKind,
    ) -> ActionResult<()> {
        let repo = gix::open_opts(fx.path(), gix::open::Options::isolated()).unwrap();
        let proof = proof();
        Writer::new(&repo, &proof).restore_file("test", fx.path(), relative, (data, kind))
    }

    /// Every name in the fixture's root, so a test can see a temporary entry left behind.
    fn root_names(fx: &rc_git_testkit::Fixture) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(fx.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn restoring_over_a_file_or_a_symlink_replaces_it_and_leaves_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let fx = rc_git_testkit::Fixture::standard();
        fx.write("plain.txt", b"old\n");
        fx.write("outside.txt", b"keep\n");
        fx.symlink("outside.txt", "link.txt");
        let before = root_names(&fx);
        restore(&fx, "plain.txt", b"new\n", EntryKind::BlobExecutable).unwrap();
        restore(&fx, "link.txt", b"fresh\n", EntryKind::Blob).unwrap();
        assert_eq!(root_names(&fx), before);
        assert_eq!(
            std::fs::read(fx.path().join("plain.txt")).unwrap(),
            b"new\n"
        );
        let mode = std::fs::metadata(fx.path().join("plain.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        let link = std::fs::symlink_metadata(fx.path().join("link.txt")).unwrap();
        assert!(
            link.is_file(),
            "the stale symlink is replaced, not followed"
        );
        assert_eq!(
            std::fs::read(fx.path().join("link.txt")).unwrap(),
            b"fresh\n"
        );
        assert_eq!(
            std::fs::read(fx.path().join("outside.txt")).unwrap(),
            b"keep\n"
        );
    }

    #[test]
    fn restoring_a_symlink_over_a_file_replaces_it_and_leaves_no_temp() {
        let fx = rc_git_testkit::Fixture::standard();
        fx.write("entry", b"old\n");
        let before = root_names(&fx);
        restore(&fx, "entry", b"target", EntryKind::Link).unwrap();
        assert_eq!(root_names(&fx), before);
        let target = std::fs::read_link(fx.path().join("entry")).unwrap();
        assert_eq!(target, Path::new("target"));
    }

    #[test]
    fn a_failed_write_leaves_the_existing_file_untouched() {
        let fx = rc_git_testkit::Fixture::standard();
        fx.write("kept.txt", b"uncommitted work\n");
        let before = root_names(&fx);
        // A NUL byte cannot be a symlink target, so the write fails after the path is chosen.
        let error = restore(&fx, "kept.txt", b"bad\0target", EntryKind::Link).unwrap_err();
        assert!(error.to_string().contains("kept.txt"), "{error}");
        assert_eq!(root_names(&fx), before);
        assert_eq!(
            std::fs::read(fx.path().join("kept.txt")).unwrap(),
            b"uncommitted work\n"
        );
    }
}
