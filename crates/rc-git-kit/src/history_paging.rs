//! Port of `HistoryPaging.swift`: a history walk plus how far it has gone, so a model can tell
//! whether it still stands where a tip's cached pages end. A page whose result the model
//! dropped as stale moves the walk past the cache; the next page then sees the mismatch and
//! opens a new walk. One page at a time: nothing here reads past the page asked for.

use std::sync::{Arc, Mutex, PoisonError};

use rc_git_core::{CommitFilter, CommitPage, CommitSummary, Oid};

use crate::repository::{HistoryWalkReading, LogCursorReading, ReadResult, RepositoryReading};

/// A log cursor plus how many commits it has handed out.
pub struct LogPager {
    cursor: Box<dyn LogCursorReading>,
    /// Commits handed out so far, or `None` once a page failed and the place is lost.
    position: Mutex<Option<usize>>,
}

impl std::fmt::Debug for LogPager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogPager").finish_non_exhaustive()
    }
}

impl LogPager {
    /// The `limit` commits after the first `start` reachable from `tip`, and the pager that
    /// now stands after them. Resumes `pager` when it stands at `start`; otherwise opens a new
    /// cursor and passes over the first `start` commits again.
    ///
    /// # Errors
    /// When the cursor cannot be opened or a page cannot be read.
    pub fn page(
        pager: Option<Arc<Self>>,
        repository: &dyn RepositoryReading,
        tip: Oid,
        start: usize,
        limit: usize,
    ) -> ReadResult<(Arc<Self>, Vec<CommitSummary>)> {
        if let Some(pager) = pager
            && let Some(page) = pager.next(start, limit)?
        {
            return Ok((pager, page));
        }
        let fresh = Arc::new(Self {
            cursor: repository.log_cursor(tip)?,
            position: Mutex::new(Some(0)),
        });
        if start > 0 {
            fresh.next(0, start)?;
        }
        // `None`: the history ended before `start`, so nothing follows it.
        let page = fresh.next(start, limit)?.unwrap_or_default();
        Ok((fresh, page))
    }

    /// The next `limit` commits, or `None` when the cursor does not stand at `start`.
    fn next(&self, start: usize, limit: usize) -> ReadResult<Option<Vec<CommitSummary>>> {
        let mut position = self.position.lock().unwrap_or_else(PoisonError::into_inner);
        if *position != Some(start) {
            return Ok(None);
        }
        *position = None;
        let page = self.cursor.next(limit)?;
        *position = Some(start + page.len());
        Ok(Some(page))
    }
}

/// A filtered history walk plus how many commits it has examined, so a model can tell whether
/// it still stands where the cached pages of a path or search end.
pub struct HistoryPager {
    walk: Box<dyn HistoryWalkReading>,
    /// Commits examined so far, or `None` once a page failed and the place is lost.
    position: Mutex<Option<usize>>,
}

impl std::fmt::Debug for HistoryPager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryPager").finish_non_exhaustive()
    }
}

/// Where a filtered page starts and how much it may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRequest {
    /// Commits the walk has examined before this page.
    pub start: usize,
    /// The most matches the page holds.
    pub limit: usize,
    /// The most commits the page examines.
    pub max_scan: usize,
}

impl HistoryPager {
    /// The next page of the walk over `tip` and `filter` after its first `request.start`
    /// examined commits, and the pager that now stands after it. Resumes `pager` when it
    /// stands at the start; otherwise opens a new walk and examines the first `start` commits
    /// again.
    ///
    /// # Errors
    /// When the walk cannot be opened or a page cannot be read.
    pub fn page(
        pager: Option<Arc<Self>>,
        repository: &dyn RepositoryReading,
        tip: Oid,
        filter: CommitFilter,
        request: PageRequest,
    ) -> ReadResult<(Arc<Self>, CommitPage)> {
        let PageRequest {
            start,
            limit,
            max_scan,
        } = request;
        if let Some(pager) = pager
            && let Some(page) = pager.next(start, limit, max_scan)?
        {
            return Ok((pager, page));
        }
        let fresh = Arc::new(Self {
            walk: repository.history_walk(tip, filter)?,
            position: Mutex::new(Some(0)),
        });
        if start > 0 {
            fresh.next(0, usize::MAX, start)?;
        }
        let page = fresh.next(start, limit, max_scan)?.unwrap_or(CommitPage {
            // The history ended before `start`: nothing follows it.
            commits: Vec::new(),
            scanned: 0,
            complete: true,
        });
        Ok((fresh, page))
    }

    /// The next page, or `None` when the walk does not stand at `start`.
    fn next(&self, start: usize, limit: usize, max_scan: usize) -> ReadResult<Option<CommitPage>> {
        let mut position = self.position.lock().unwrap_or_else(PoisonError::into_inner);
        if *position != Some(start) {
            return Ok(None);
        }
        *position = None;
        let page = self.walk.next(limit, max_scan)?;
        *position = Some(start + page.scanned);
        Ok(Some(page))
    }
}
