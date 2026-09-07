//! Line-by-line attribution of a file to the commits that introduced its lines.

use crate::error::{Result, object};
use crate::oid::Oid;
use crate::repo::Repository;

/// A run of consecutive lines introduced by one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameRange {
    /// The commit that introduced the lines.
    pub commit: Oid,
    /// 1-based first line of the run in the blamed file.
    pub start_line: u32,
    /// Number of lines in the run.
    pub line_count: u32,
}

impl Repository {
    /// Attributes every line of the file at `path` in the commit `tip` to the commit that
    /// introduced it, as consecutive ranges ordered by line number.
    ///
    /// This is the one read that walks history: its cost grows with the number of commits
    /// that touched the file, so call it on demand for the file on screen, never in bulk.
    ///
    /// # Errors
    /// [`Error::Object`](crate::Error::Object) when `tip` is not a commit, the path does not
    /// exist in it, or the walk fails.
    pub fn blame(&self, tip: Oid, path: &str) -> Result<Vec<BlameRange>> {
        self.with(|repo| {
            let outcome = repo
                .blame_file(
                    path.into(),
                    *tip.as_gix(),
                    gix::repository::blame_file::Options::default(),
                )
                .map_err(object("blame file", tip))?;
            let mut ranges: Vec<BlameRange> = Vec::with_capacity(outcome.entries.len());
            for entry in outcome.entries {
                ranges.push(BlameRange {
                    commit: Oid::from_gix(entry.commit_id),
                    start_line: entry.start_in_blamed_file + 1,
                    line_count: entry.len.get(),
                });
            }
            ranges.sort_by_key(|range| range.start_line);
            Ok(ranges)
        })
    }
}
