//! Working-tree file writes for the guard's restore paths, the hunk splice shared by
//! partial commits and partial discards, and author identity resolution. Everything works
//! on raw bytes: gitattributes filters and eol conversion are not applied, matching how the
//! core compares and diffs.

use std::ops::Range;
use std::path::{Path, PathBuf};

use gix::objs::tree::EntryKind;

use super::{ActionError, ActionResult, Author, HunkStamp, Identity, git_err};

/// The identity to record: [`Author::explicit`], else the repository's own
/// `user.name`/`user.email` (the isolated open reads exactly that file), else
/// [`Author::fallback`].
pub(super) fn resolve_identity(repo: &gix::Repository, author: Author) -> ActionResult<Identity> {
    author
        .explicit
        .or_else(|| repository_identity(repo))
        .or(author.fallback)
        .ok_or(ActionError::MissingIdentity)
}

/// The repository's own `user.name`/`user.email`, when both are set and non-empty.
pub(super) fn repository_identity(repo: &gix::Repository) -> Option<Identity> {
    let config = repo.config_snapshot();
    let name = config.string("user.name")?;
    let email = config.string("user.email")?;
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some(Identity {
        name: name.to_string(),
        email: email.to_string(),
    })
}

/// The path rules git applies before it writes a tree entry to disk: `core.protectHFS` and
/// `core.protectNTFS` both on, as git's own defaults are on macOS.
const PATH_RULES: gix::validate::path::component::Options =
    gix::validate::path::component::Options {
        protect_windows: false,
        protect_hfs: true,
        protect_ntfs: true,
    };

/// Refuses a repository-relative path git itself refuses to check out: an empty path, an
/// empty, `.` or `..` segment (so also a leading, trailing or doubled `/`), and a segment
/// that is `.git` in any spelling HFS+ or NTFS resolve to it. `symlink` marks the last
/// segment as a symlink, which also refuses a symlinked `.gitmodules`. Nothing is
/// normalized: a path that needs it is refused.
///
/// # Errors
/// [`ActionError::Git`] naming `op`, the path, and the rule it breaks.
pub(super) fn validate_path(op: &'static str, path: &str, symlink: bool) -> ActionResult<()> {
    use gix::bstr::ByteSlice;
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        let mode = if symlink && segments.peek().is_none() {
            Some(gix::validate::path::component::Mode::Symlink)
        } else {
            None
        };
        gix::validate::path::component(segment.as_bytes().as_bstr(), mode, PATH_RULES).map_err(
            |error| ActionError::git(op, path, format!("not a valid git path: {error}")),
        )?;
    }
    Ok(())
}

/// Joins a repository-relative path under `work_dir` after [`validate_path`], refusing a
/// path that passes through a symlinked directory (git's "beyond a symbolic link" rule), so
/// nothing outside the working tree is ever read, written, or deleted through one.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and the path, for an invalid path, a symlinked parent,
/// or a parent that cannot be inspected.
pub(super) fn join_under(
    work_dir: &Path,
    relative: &str,
    op: &'static str,
) -> ActionResult<PathBuf> {
    validate_path(op, relative, false)?;
    let mut parent = work_dir.to_path_buf();
    let mut walked = 0;
    let mut segments = relative.split('/').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            break;
        }
        parent.push(segment);
        walked += segment.len() + 1;
        match std::fs::symlink_metadata(&parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(crosses_link(op, relative, &relative[..walked - 1]));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(ActionError::git(op, relative, error)),
        }
    }
    Ok(work_dir.join(relative))
}

/// Runs [`join_under`] over every path of a selection, so a bad one refuses the whole
/// action before anything is read or written.
///
/// # Errors
/// [`ActionError::Git`] for the first path [`join_under`] refuses.
pub(super) fn check_paths<'a>(
    work_dir: &Path,
    paths: impl IntoIterator<Item = &'a str>,
    op: &'static str,
) -> ActionResult<()> {
    for path in paths {
        join_under(work_dir, path, op)?;
    }
    Ok(())
}

/// Refuses deleting `relative` as untracked unless its directory holds an entry whose name
/// is byte-equal to the last segment. A case-insensitive volume would otherwise resolve
/// `A.TXT` to the tracked `a.txt` and delete it.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and the path when no entry has exactly that name, or
/// the directory cannot be read.
pub(super) fn require_exact_name(
    work_dir: &Path,
    relative: &str,
    op: &'static str,
) -> ActionResult<()> {
    use std::os::unix::ffi::OsStrExt;
    let full = join_under(work_dir, relative, op)?;
    let missing = || {
        ActionError::git(
            op,
            relative,
            "not in HEAD and no file has exactly this name",
        )
    };
    let Some(parent) = full.parent() else {
        return Err(missing());
    };
    let name = relative.rsplit('/').next().unwrap_or(relative);
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(missing()),
        Err(error) => return Err(ActionError::git(op, relative, error)),
    };
    for entry in entries {
        let entry = entry.map_err(git_err(op, relative))?;
        if entry.file_name().as_bytes() == name.as_bytes() {
            return Ok(());
        }
    }
    Err(missing())
}

/// What sits at a working-tree path, read the way git hashes it.
pub(super) enum DiskEntry {
    /// Nothing exists at the path.
    Absent,
    /// A directory, which no file entry can match.
    Directory,
    /// A symlink's target (`EntryKind::Link`), or a file's bytes with its executable bit
    /// (`EntryKind::BlobExecutable` or `EntryKind::Blob`).
    Present { data: Vec<u8>, kind: EntryKind },
}

/// Reads `relative` below `work_dir` without following a symlink at the path itself: a
/// symlink yields its target, a regular file its bytes and the owner's executable bit.
/// The one reader every guard check uses, so they agree on what counts as a change.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and the path for an invalid or link-crossing path, and
/// for any failure to inspect or read it other than its absence, with the OS error kept.
pub(super) fn disk_entry(
    work_dir: &Path,
    relative: &str,
    op: &'static str,
) -> ActionResult<DiskEntry> {
    use std::os::unix::fs::PermissionsExt;
    let full = join_under(work_dir, relative, op)?;
    let absent_or_error = |error: std::io::Error| {
        // A file where a parent directory was (`a` for `a/b`) means `a/b` is gone too.
        if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ) {
            Ok(DiskEntry::Absent)
        } else {
            Err(ActionError::git(op, relative, error))
        }
    };
    let meta = match std::fs::symlink_metadata(&full) {
        Ok(meta) => meta,
        Err(error) => return absent_or_error(error),
    };
    if meta.is_dir() {
        return Ok(DiskEntry::Directory);
    }
    if meta.file_type().is_symlink() {
        return match std::fs::read_link(&full) {
            Ok(target) => Ok(DiskEntry::Present {
                data: target.into_os_string().into_encoded_bytes(),
                kind: EntryKind::Link,
            }),
            Err(error) => absent_or_error(error),
        };
    }
    // Only the owner's bit counts, as in git: group- or other-executable alone is a blob.
    let kind = if meta.permissions().mode() & 0o100 == 0 {
        EntryKind::Blob
    } else {
        EntryKind::BlobExecutable
    };
    match std::fs::read(&full) {
        Ok(data) => Ok(DiskEntry::Present { data, kind }),
        Err(error) => absent_or_error(error),
    }
}

/// Writes `data` to `relative` below `work_dir` as `kind` describes: a symlink's blob holds
/// its target, and the executable bit follows the mode. Any existing file, or an empty
/// directory, is removed first, so writing never follows a stale symlink. Errors name `op`,
/// the action the write is for.
pub(super) fn restore_file(
    op: &'static str,
    work_dir: &Path,
    relative: &str,
    data: &[u8],
    kind: EntryKind,
) -> ActionResult<()> {
    validate_path(op, relative, kind == EntryKind::Link)?;
    let full = join_under(work_dir, relative, op)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    {
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(git_err(op, relative))?;
        }
        let removed = match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir(&full),
            Ok(_) => std::fs::remove_file(&full),
            Err(error) => Err(error),
        };
        match removed {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ActionError::git(op, relative, error)),
        }
        match kind {
            EntryKind::Link => {
                let target: &std::ffi::OsStr = {
                    use std::os::unix::ffi::OsStrExt;
                    std::ffi::OsStr::from_bytes(data)
                };
                std::os::unix::fs::symlink(target, &full).map_err(git_err(op, relative))?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => {
                std::fs::write(&full, data).map_err(git_err(op, relative))?;
                if kind == EntryKind::BlobExecutable {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755))
                        .map_err(git_err(op, relative))?;
                }
            }
            EntryKind::Tree | EntryKind::Commit => {
                return Err(ActionError::git(
                    op,
                    relative,
                    "only files and symlinks can be restored",
                ));
            }
        }
    }
    Ok(())
}

/// Deletes `relative` below `work_dir`, then removes each parent directory the deletion left
/// empty, as git does; a file already gone is not an error, so a retried action lands in the
/// state it retries from. Errors name `op`, the action the delete is for.
pub(super) fn delete_file(op: &'static str, work_dir: &Path, relative: &str) -> ActionResult<()> {
    let full = join_under(work_dir, relative, op)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    match std::fs::remove_file(&full) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ActionError::git(op, relative, error)),
    }
    prune_empty_parents(work_dir, relative);
    Ok(())
}

/// Removes the parent directories of `relative`, deepest first and never `work_dir` itself,
/// stopping at the first one that cannot be removed. Like git's `schedule_dir_for_removal`,
/// a failure here means "still in use" (not empty, or already gone), never an error: the
/// file the caller deleted is gone either way.
fn prune_empty_parents(work_dir: &Path, relative: &str) {
    for (end, _) in relative.rmatch_indices('/') {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
        )]
        let removed = std::fs::remove_dir(work_dir.join(&relative[..end]));
        if removed.is_err() {
            break;
        }
    }
}

/// The refusal for `relative` passing through the symlink `link`.
pub(super) fn crosses_link(op: &'static str, relative: &str, link: &str) -> ActionError {
    ActionError::git(
        op,
        relative,
        format!("crosses a symbolic link at '{link}', which would leave the working tree"),
    )
}

/// Applies the `selected` hunks of the diff `old` -> `new` onto `old`: the result carries
/// exactly those hunks' changes. Context lines are equal on both sides of a hunk, so
/// replacing the old header range with the new header range is byte-exact, trailing
/// newlines included. Stamps must be in file order and inside both buffers; anything else
/// means the file moved under the selection and the caller maps the error to
/// [`ActionError::HunkMismatch`].
pub(super) fn splice(
    old: &[u8],
    new: &[u8],
    selected: &[HunkStamp],
) -> Result<Vec<u8>, &'static str> {
    let old_bounds = line_bounds(old);
    let new_bounds = line_bounds(new);
    let mut out = Vec::with_capacity(new.len());
    let mut cursor = 0usize;
    for stamp in selected {
        let old_range = line_range(&old_bounds, stamp.old_start, stamp.old_lines)
            .ok_or("a hunk lies outside the tree side of the file")?;
        let new_range = line_range(&new_bounds, stamp.new_start, stamp.new_lines)
            .ok_or("a hunk lies outside the disk side of the file")?;
        if old_range.start < cursor {
            return Err("hunks overlap or are out of order");
        }
        out.extend_from_slice(&old[cursor..old_range.start]);
        out.extend_from_slice(&new[new_range]);
        cursor = old_range.end;
    }
    out.extend_from_slice(&old[cursor..]);
    Ok(out)
}

/// Byte offset where each 1-based line starts, with a final sentinel at the buffer end.
fn line_bounds(data: &[u8]) -> Vec<usize> {
    let mut bounds = vec![0];
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' && index + 1 < data.len() {
            bounds.push(index + 1);
        }
    }
    bounds.push(data.len());
    bounds
}

/// The byte range of `count` lines starting at 1-based `start`. A zero-length range sits
/// after line `start`, matching the unified-diff header convention.
fn line_range(bounds: &[usize], start: u32, count: u32) -> Option<Range<usize>> {
    let start = start as usize;
    let count = count as usize;
    if count == 0 {
        let at = *bounds.get(start)?;
        return Some(at..at);
    }
    let begin = *bounds.get(start.checked_sub(1)?)?;
    let end = *bounds.get(start - 1 + count)?;
    Some(begin..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> HunkStamp {
        HunkStamp {
            old_start,
            old_lines,
            new_start,
            new_lines,
        }
    }

    #[test]
    fn selecting_no_hunks_returns_the_old_bytes() {
        assert_eq!(splice(b"a\nb\n", b"a\nX\n", &[]).unwrap(), b"a\nb\n");
    }

    #[test]
    fn a_modification_hunk_replaces_its_lines() {
        let out = splice(b"a\nb\nc\n", b"a\nX\nc\n", &[stamp(1, 3, 1, 3)]).unwrap();
        assert_eq!(out, b"a\nX\nc\n");
    }

    /// The stamps rc-git-core produces for `old` -> `new` at `context`, so a change in the
    /// producer's numbering convention fails the splice tests that use them.
    fn core_stamps(old: &[u8], new: &[u8], context: u32) -> Vec<HunkStamp> {
        let fx = rc_git_testkit::Fixture::standard();
        fx.write("old.txt", old);
        fx.write("new.txt", new);
        let blob = |path| {
            let hex = fx.git_output(&["hash-object", "-w", path]);
            rc_git_core::Oid::from_hex(&hex).unwrap()
        };
        let (old, new) = (blob("old.txt"), blob("new.txt"));
        let repo = rc_git_core::Repository::open(fx.path()).unwrap();
        let diff = repo
            .diff_blobs(
                Some(old),
                Some(new),
                rc_git_core::DEFAULT_BLOB_LIMIT,
                context,
            )
            .unwrap();
        let rc_git_core::BlobDiff::Text { hunks } = diff else {
            panic!("expected a text diff, got {diff:?}");
        };
        hunks.iter().map(HunkStamp::from).collect()
    }

    #[test]
    fn an_insertion_hunk_lands_after_its_anchor_line() {
        let (old, new) = (b"a\nb\n", b"a\nX\nb\n");
        for context in [0, rc_git_core::DEFAULT_DIFF_CONTEXT] {
            let stamps = core_stamps(old, new, context);
            assert_eq!(splice(old, new, &stamps).unwrap(), new, "context {context}");
        }
        assert_eq!(core_stamps(old, new, 0), [stamp(1, 0, 2, 1)]);
    }

    #[test]
    fn a_context_free_deletion_hunk_removes_its_line() {
        let (old, new) = (b"a\nb\nc\n", b"a\nc\n");
        let stamps = core_stamps(old, new, 0);
        assert_eq!(stamps, [stamp(2, 1, 1, 0)]);
        assert_eq!(splice(old, new, &stamps).unwrap(), new);
    }

    #[test]
    fn a_deletion_hunk_removes_its_lines() {
        let out = splice(b"a\nb\nc\n", b"a\nc\n", &[stamp(2, 1, 1, 0)]).unwrap();
        assert_eq!(out, b"a\nc\n");
    }

    #[test]
    fn one_of_two_hunks_applies_alone() {
        let old = b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = b"a\nX\nc\nd\ne\nf\ng\nh\ni\nY\nj\n";
        // Hunk 1 changes line 2; hunk 2 inserts before line 10 (context 1 for compactness).
        let first = stamp(1, 3, 1, 3);
        let second = stamp(9, 2, 9, 3);
        let only_first = splice(old, new, &[first]).unwrap();
        assert_eq!(only_first, b"a\nX\nc\nd\ne\nf\ng\nh\ni\nj\n");
        let only_second = splice(old, new, &[second]).unwrap();
        assert_eq!(only_second, b"a\nb\nc\nd\ne\nf\ng\nh\ni\nY\nj\n");
        let both = splice(old, new, &[first, second]).unwrap();
        assert_eq!(both, new);
    }

    #[test]
    fn a_missing_trailing_newline_survives_the_splice() {
        let out = splice(b"a\nb", b"a\nc", &[stamp(2, 1, 2, 1)]).unwrap();
        assert_eq!(out, b"a\nc");
    }

    #[test]
    fn out_of_order_or_out_of_range_stamps_are_refused() {
        let old = b"a\nb\nc\n";
        let new = b"a\nX\nY\n";
        assert!(splice(old, new, &[stamp(3, 1, 3, 1), stamp(2, 1, 2, 1)]).is_err());
        assert!(splice(old, new, &[stamp(9, 1, 1, 1)]).is_err());
        assert!(splice(old, new, &[stamp(1, 1, 9, 1)]).is_err());
    }
}
