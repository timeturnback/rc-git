//! The stash as a read-only list: one entry per `refs/stash` reflog line, newest first.

use crate::error::{Result, git};
use crate::oid::Oid;
use crate::repo::Repository;

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
    /// repository with no stash returns an empty list. Reads one reflog file; nothing
    /// scales with stash size beyond its line count.
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
            let Some(lines) = platform.all().map_err(git("read stash reflog"))? else {
                return Ok(Vec::new());
            };
            // The reflog file is oldest first; the newest line is `stash@{0}`.
            let mut entries = Vec::new();
            for line in lines {
                let line = line.map_err(git("decode stash reflog"))?;
                entries.push(StashEntry {
                    index: 0,
                    oid: Oid::from_gix(line.new_oid()),
                    message: line.message.to_string(),
                });
            }
            entries.reverse();
            for (index, entry) in entries.iter_mut().enumerate() {
                entry.index = index;
            }
            Ok(entries)
        })
    }
}
