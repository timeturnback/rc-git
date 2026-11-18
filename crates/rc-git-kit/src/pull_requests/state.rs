//! The pull request list and one pull request's tab as state machines, free of GPUI so their
//! rules are tested directly: one load in flight and at most one queued behind it (a burst of
//! refreshes costs one more load, never a pile-up), a generation on every load so an answer
//! for anything but the newest is dropped, and a failed refresh that keeps what was read but
//! marks it stale rather than presenting it as current.

use std::collections::HashSet;

use super::{PageCursor, PullRequestDetail, PullRequestStamp, PullRequestSummary, ReadError};

/// The most pages the list holds: [`super::PageCursor`] paging stops there.
pub const MAX_PAGES: usize = 10;

/// How current what is on screen is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    /// Nothing read yet; the first read runs.
    Loading,
    /// The last read succeeded, at this time (seconds since the Unix epoch).
    Current {
        /// When.
        at: u64,
    },
    /// The last read failed. Whatever an earlier read left is stale: never shown as current.
    Failed(ReadError),
}

/// What a list load reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListLoadKind {
    /// The first page again, dropping every later page.
    Refresh,
    /// The page after the last one held.
    More,
}

/// One list load to run, under its generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLoad {
    /// Answer [`ListState::finish`] with it.
    pub generation: u64,
    /// The cursor to read from; `None` for the first page.
    pub after: Option<PageCursor>,
}

/// The open pull requests of one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListState {
    generation: u64,
    in_flight: Option<ListLoadKind>,
    queued: Option<ListLoadKind>,
    rows: Vec<PullRequestSummary>,
    next: Option<PageCursor>,
    pages: usize,
    freshness: Freshness,
}

impl Default for ListState {
    fn default() -> Self {
        Self::new()
    }
}

impl ListState {
    /// A list that has read nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generation: 0,
            in_flight: None,
            queued: None,
            rows: Vec::new(),
            next: None,
            pages: 0,
            freshness: Freshness::Loading,
        }
    }

    /// Forgets everything for a new repository or account. A load still running belongs to
    /// the old one: its answer is dropped.
    pub fn reset(&mut self) {
        let generation = self.generation + 1;
        *self = Self::new();
        self.generation = generation;
    }

    /// The pull requests read, most recently updated first.
    #[must_use]
    pub fn rows(&self) -> &[PullRequestSummary] {
        &self.rows
    }

    /// How current [`ListState::rows`] are.
    #[must_use]
    pub fn freshness(&self) -> &Freshness {
        &self.freshness
    }

    /// Whether a load runs.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Whether Load More can read another page.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.next.is_some() && self.pages < MAX_PAGES
    }

    /// How many pages are held.
    #[must_use]
    pub fn pages(&self) -> usize {
        self.pages
    }

    /// Asks for the first page again. Runs now when nothing runs; else it waits behind the
    /// running load, as the one queued load (a refresh outranks a queued Load More).
    pub fn refresh(&mut self) -> Option<ListLoad> {
        self.request(ListLoadKind::Refresh)
    }

    /// Asks for the next page, when there is one. Ignored while another load runs.
    pub fn load_more(&mut self) -> Option<ListLoad> {
        if !self.has_more() || self.in_flight.is_some() {
            return None;
        }
        self.request(ListLoadKind::More)
    }

    fn request(&mut self, kind: ListLoadKind) -> Option<ListLoad> {
        if self.in_flight.is_some() {
            if self.queued != Some(ListLoadKind::Refresh) {
                self.queued = Some(kind);
            }
            return None;
        }
        self.start(kind)
    }

    fn start(&mut self, kind: ListLoadKind) -> Option<ListLoad> {
        let after = match kind {
            ListLoadKind::Refresh => None,
            ListLoadKind::More => Some(self.next.clone().filter(|_| self.has_more())?),
        };
        self.generation += 1;
        self.in_flight = Some(kind);
        Some(ListLoad {
            generation: self.generation,
            after,
        })
    }

    /// Takes the answer of load `generation`, read at `now`. An answer for any other
    /// generation is dropped. Answers the queued load to run next, if any.
    pub fn finish(
        &mut self,
        generation: u64,
        answer: Result<super::PullRequestPage, ReadError>,
        now: u64,
    ) -> Option<ListLoad> {
        if generation != self.generation {
            return None;
        }
        let kind = self.in_flight.take()?;
        match answer {
            Ok(page) => {
                if kind == ListLoadKind::Refresh {
                    self.rows.clear();
                    self.pages = 0;
                }
                let held: HashSet<u64> = self.rows.iter().map(|row| row.number).collect();
                self.rows.extend(
                    page.pull_requests
                        .into_iter()
                        .filter(|row| !held.contains(&row.number)),
                );
                self.pages += 1;
                self.next = page.next;
                self.freshness = Freshness::Current { at: now };
            }
            Err(ReadError::Cancelled) => {}
            Err(error) => self.freshness = Freshness::Failed(error),
        }
        let queued = self.queued.take()?;
        self.start(queued)
    }
}

/// One detail load to run, under its tab's epoch and its generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailLoad {
    /// The pull request.
    pub number: u64,
    /// The tab it was asked for ([`DetailState::opened`]).
    pub epoch: u64,
    /// Its generation within that tab.
    pub generation: u64,
}

/// One open pull request tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailState {
    number: u64,
    epoch: u64,
    generation: u64,
    in_flight: bool,
    queued: bool,
    detail: Option<PullRequestDetail>,
    freshness: Freshness,
}

impl DetailState {
    /// The tab of pull request `number`, read nothing yet, in epoch 0.
    #[must_use]
    pub fn new(number: u64) -> Self {
        Self::opened(number, 0)
    }

    /// The tab of pull request `number`, read nothing yet, opened as `epoch`: a number its
    /// window never gave another tab. A read asked for another tab of the same number (one that
    /// was closed, or one from before the repository's link changed) carries that tab's epoch,
    /// so its answer is dropped here however its generation compares.
    #[must_use]
    pub fn opened(number: u64, epoch: u64) -> Self {
        Self {
            number,
            epoch,
            generation: 0,
            in_flight: false,
            queued: false,
            detail: None,
            freshness: Freshness::Loading,
        }
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The last detail read, current or stale per [`DetailState::freshness`].
    #[must_use]
    pub fn detail(&self) -> Option<&PullRequestDetail> {
        self.detail.as_ref()
    }

    /// How current [`DetailState::detail`] is.
    #[must_use]
    pub fn freshness(&self) -> &Freshness {
        &self.freshness
    }

    /// Whether a read runs.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.in_flight
    }

    /// What the user sees, for an action to compare against a fresh read: `None` unless the
    /// detail on screen is current, so a stale screen never stands for what was seen.
    #[must_use]
    pub fn stamp(&self) -> Option<PullRequestStamp> {
        match self.freshness {
            Freshness::Current { .. } => self.detail.as_ref().map(PullRequestDetail::stamp),
            Freshness::Loading | Freshness::Failed(_) => None,
        }
    }

    /// Whether the current detail has a check still running, so its tab polls.
    #[must_use]
    pub fn has_pending_checks(&self) -> bool {
        let Some(detail) = &self.detail else {
            return false;
        };
        matches!(self.freshness, Freshness::Current { .. })
            && super::checks::pending(&detail.checks.checks)
    }

    /// Asks for a fresh read. Runs now when none runs, else waits as the one queued read.
    pub fn refresh(&mut self) -> Option<DetailLoad> {
        if self.in_flight {
            self.queued = true;
            return None;
        }
        Some(self.start())
    }

    fn start(&mut self) -> DetailLoad {
        self.generation += 1;
        self.in_flight = true;
        DetailLoad {
            number: self.number,
            epoch: self.epoch,
            generation: self.generation,
        }
    }

    /// Takes the answer of `load`, read at `now`; an answer for another tab (another epoch) or
    /// another generation is dropped. Answers the queued read to run next, if any.
    pub fn finish(
        &mut self,
        load: &DetailLoad,
        answer: Result<PullRequestDetail, ReadError>,
        now: u64,
    ) -> Option<DetailLoad> {
        let current = load.number == self.number
            && load.epoch == self.epoch
            && load.generation == self.generation;
        if !current || !self.in_flight {
            return None;
        }
        self.in_flight = false;
        match answer {
            Ok(detail) => {
                self.detail = Some(detail);
                self.freshness = Freshness::Current { at: now };
            }
            Err(ReadError::Cancelled) => {}
            Err(error) => self.freshness = Freshness::Failed(error),
        }
        if std::mem::take(&mut self.queued) {
            return Some(self.start());
        }
        None
    }
}
