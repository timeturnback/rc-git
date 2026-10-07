//! The stash as a read-only list: one entry per `refs/stash` reflog line, newest first.

use crate::error::{Result, git};
use crate::oid::Oid;
use crate::repo::Repository;

/// One readable line of the `refs/stash` reflog, as git's files backend reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashReflogLine {
    /// Position of the line among the file's `\n`-separated lines, counting from 0.
    pub line: usize,
    /// The stash commit the line records.
    pub oid: Oid,
    /// The reflog message.
    pub message: String,
}

/// The readable lines of a raw `refs/stash` reflog, oldest first. Like git's files backend, a
/// line that does not decode and a final line without its `\n` (a cut-short append) are
/// skipped, so the n-th entry from the end is git's `stash@{n}`. Every caller that maps a
/// stash index to a reflog line goes through here.
#[must_use]
pub fn stash_reflog_lines(raw: &[u8]) -> Vec<StashReflogLine> {
    let pieces: Vec<&[u8]> = raw.split(|byte| *byte == b'\n').collect();
    // The last piece is empty after a final newline, or an unterminated line: never valid.
    let terminated = pieces.len().saturating_sub(1);
    pieces
        .iter()
        .take(terminated)
        .enumerate()
        .filter_map(|(line, bytes)| {
            let parsed = gix::refs::file::log::LineRef::from_bytes(bytes).ok()?;
            Some(StashReflogLine {
                line,
                oid: Oid::from_gix(parsed.new_oid()),
                message: parsed.message.to_string(),
            })
        })
        .collect()
}

/// One stashed change set, as `git stash list` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// Position in the stash, so `index` 0 is `stash@{0}`, the newest.
    pub index: usize,
    /// The stash commit.
    pub oid: Oid,
    /// The reflog message, e.g. `WIP on main: 1a2b3c4 Subject`.
    pub message: String,
}

impl Repository {
    /// Lists every stash entry, newest first, by reading the `refs/stash` reflog. A
    /// repository with no stash, or a stash ref without a reflog, returns an empty list.
    /// Unreadable reflog lines are skipped as git skips them; see [`stash_reflog_lines`].
    /// Reads one reflog file; nothing scales with stash size beyond its line count.
    ///
    /// # Errors
    /// [`Error::Git`](crate::Error::Git) when the reference or its reflog cannot be read.
    pub fn stash_list(&self) -> Result<Vec<StashEntry>> {
        self.with(|repo| {
            let Some(reference) = repo
                .try_find_reference("refs/stash")
                .map_err(git("read stash reference"))?
            else {
                return Ok(Vec::new());
            };
            let mut platform = reference.log_iter();
            if platform.all().map_err(git("read stash reflog"))?.is_none() {
                return Ok(Vec::new());
            }
            let valid = stash_reflog_lines(&platform.buf);
            let entries = valid
                .into_iter()
                .rev()
                .enumerate()
                .map(|(index, line)| StashEntry {
                    index,
                    oid: line.oid,
                    message: line.message,
                })
                .collect();
            Ok(entries)
        })
    }
}
