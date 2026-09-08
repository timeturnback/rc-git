//! Working-tree file writes for the guard's restore paths, the hunk splice shared by
//! partial commits and partial discards, and author identity resolution. Everything works
//! on raw bytes: gitattributes filters and eol conversion are not applied, matching how the
//! core compares and diffs.

use std::ops::Range;
use std::path::{Path, PathBuf};

use gix::objs::tree::EntryKind;

use super::{ActionError, ActionResult, HunkStamp, Identity};

/// The author to record: the explicit one when given, else the repository's own
/// `user.name`/`user.email` (the isolated open reads exactly that file).
pub(super) fn resolve_identity(
    repo: &gix::Repository,
    explicit: Option<Identity>,
) -> ActionResult<Identity> {
    if let Some(identity) = explicit {
        return Ok(identity);
    }
    let config = repo.config_snapshot();
    let name = config.string("user.name");
    let email = config.string("user.email");
    match (name, email) {
        (Some(name), Some(email)) if !name.is_empty() && !email.is_empty() => Ok(Identity {
            name: name.to_string(),
            email: email.to_string(),
        }),
        (Some(_) | None, Some(_) | None) => Err(ActionError::MissingIdentity),
    }
}

/// Joins a repository-relative path under `work_dir`, refusing traversal outside it.
pub(super) fn join_under(work_dir: &Path, relative: &str) -> ActionResult<PathBuf> {
    let mut joined = work_dir.to_path_buf();
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(file_error(
                "restore file",
                relative,
                "path may not leave the working tree".into(),
            ));
        }
        joined.push(segment);
    }
    Ok(joined)
}

/// Writes `data` to `relative` below `work_dir` as `kind` describes: a symlink's blob holds
/// its target, and the executable bit follows the mode. Any existing file is removed first,
/// so writing never follows a stale symlink.
pub(super) fn restore_file(
    work_dir: &Path,
    relative: &str,
    data: &[u8],
    kind: EntryKind,
) -> ActionResult<()> {
    const OP: &str = "restore file";
    let full = join_under(work_dir, relative)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    {
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| file_error(OP, relative, Box::new(source)))?;
        }
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(file_error(OP, relative, Box::new(error))),
        }
        match kind {
            EntryKind::Link => {
                let target: &std::ffi::OsStr = {
                    use std::os::unix::ffi::OsStrExt;
                    std::ffi::OsStr::from_bytes(data)
                };
                std::os::unix::fs::symlink(target, &full)
                    .map_err(|source| file_error(OP, relative, Box::new(source)))?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => {
                std::fs::write(&full, data)
                    .map_err(|source| file_error(OP, relative, Box::new(source)))?;
                if kind == EntryKind::BlobExecutable {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755))
                        .map_err(|source| file_error(OP, relative, Box::new(source)))?;
                }
            }
            EntryKind::Tree | EntryKind::Commit => {
                return Err(file_error(
                    OP,
                    relative,
                    "only files and symlinks can be restored".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Deletes `relative` below `work_dir`; a file already gone is not an error, so a retried
/// action lands in the state it retries from.
pub(super) fn delete_file(work_dir: &Path, relative: &str) -> ActionResult<()> {
    let full = join_under(work_dir, relative)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    match std::fs::remove_file(&full) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(file_error("delete file", relative, Box::new(error))),
    }
}

fn file_error(
    op: &'static str,
    path: &str,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
) -> ActionError {
    ActionError::Git {
        op,
        name: path.to_owned(),
        source,
    }
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

    #[test]
    fn an_insertion_hunk_lands_after_its_anchor_line() {
        let out = splice(b"a\nb\n", b"a\nX\nb\n", &[stamp(1, 0, 2, 1)]).unwrap();
        assert_eq!(out, b"a\nX\nb\n");
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
