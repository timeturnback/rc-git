//! Commit search over the anchor's history, streamed in scan chunks that start small and
//! double, so a query replaced while typing stops after a short walk.

use std::sync::Arc;

use gpui::{Context, Task};
use rc_git_core::{CommitFilter, CommitPage};

use super::{
    BrowserEvent, BrowserModel, HISTORY_PAGE_SIZE, SEARCH_FIRST_SCAN_CHUNK, SEARCH_SCAN_CHUNK,
    call, filter_is_empty, with,
};
use crate::back_forward::HistoryScope;
use crate::blocking::blocking;
use crate::history_paging::{HistoryPager, PageRequest};
use crate::repository::ReadResult;

/// What one round of a search walk does next.
enum SearchStep {
    Stale,
    Done,
    Read(Task<ReadResult<(Arc<HistoryPager>, CommitPage)>>),
}

impl BrowserModel {
    /// Starts a search over the anchor's history, streaming matches in scan chunks. Under a
    /// path scope it searches only the commits that changed that path. An empty filter clears
    /// instead. A newer search, scope change or ref switch between chunks drops the rest of
    /// the walk, and [`BrowserEvent::SearchChanged`] reports the drop. Asked for before the
    /// history has an anchor, the filter waits and runs once a ref or tree is shown.
    pub fn search(&mut self, filter: CommitFilter, cx: &mut Context<Self>) -> Task<()> {
        if self.repository.is_none() {
            return Task::ready(());
        }
        if self.history_anchor.is_none() {
            self.pending_search = (!filter_is_empty(&filter)).then_some(filter);
            return Task::ready(());
        }
        if filter_is_empty(&filter) {
            self.clear_search(cx);
            return Task::ready(());
        }
        let mut filter = filter;
        if let HistoryScope::Path(path) = &self.history_scope {
            filter.path = Some(path.clone());
        }
        self.generations.search += 1;
        self.search_pager = None;
        self.search_filter = Some(filter);
        self.search_results.clear();
        self.search_scanned = 0;
        self.search_complete = false;
        self.search_error = None;
        self.is_searching = true;
        Self::changed(BrowserEvent::SearchChanged, cx);
        self.continue_search(HISTORY_PAGE_SIZE, cx)
    }

    /// Fetches the next page of matches. Refuses after a failed chunk: only a new filter
    /// searches again.
    pub fn load_more_search_results(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.search_filter.is_none()
            || self.is_searching
            || self.search_complete
            || self.search_error.is_some()
        {
            return Task::ready(());
        }
        self.is_searching = true;
        Self::changed(BrowserEvent::SearchChanged, cx);
        self.continue_search(self.search_results.len() + HISTORY_PAGE_SIZE, cx)
    }

    /// Drops the active search, and one waiting for an anchor; the plain history is still
    /// loaded and shows again at once.
    pub fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.pending_search = None;
        self.generations.search += 1;
        self.search_pager = None;
        let had_search = self.search_filter.take().is_some();
        self.search_results.clear();
        self.search_scanned = 0;
        self.search_complete = false;
        self.search_error = None;
        self.is_searching = false;
        if had_search {
            Self::changed(BrowserEvent::SearchChanged, cx);
        }
    }

    /// Resolves a revision (an abbreviated hash, a ref name, anything rev-parse accepts) and
    /// shows that commit's detail. `false`, reporting nothing, when the spec does not resolve
    /// or another commit was picked during the read: the search box submits any hex-looking
    /// word as a revision while the message search for it already runs.
    pub fn jump_to_revision(&mut self, spec: &str, cx: &mut Context<Self>) -> Task<bool> {
        let Some(repository) = self.repository.clone() else {
            return Task::ready(false);
        };
        let generation = self.generations.diff;
        let spec = spec.to_owned();
        let resolve = blocking(cx.background_executor(), move || repository.resolve(&spec));
        Self::run(cx, false, async move |this, cx| {
            let resolved = resolve.await;
            let current = with(&this, cx, |model, _| generation == model.generations.diff);
            if current != Some(true) {
                return false;
            }
            let Ok(oid) = resolved else {
                return false;
            };
            call(&this, cx, |model, cx| model.jump_to_commit(oid, cx)).await;
            true
        })
    }

    /// Runs the search typed before the history had an anchor, once one is set.
    pub(super) fn run_pending_search(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.history_anchor.is_none() {
            return Task::ready(());
        }
        let Some(filter) = self.pending_search.take() else {
            return Task::ready(());
        };
        self.search(filter, cx)
    }

    fn continue_search(&mut self, target: usize, cx: &mut Context<Self>) -> Task<()> {
        let generation = self.generations.search;
        Self::run(cx, (), async move |this, cx| {
            let mut chunk = SEARCH_FIRST_SCAN_CHUNK;
            loop {
                let step = with(&this, cx, |model, cx| {
                    model.next_search_chunk(generation, target, chunk, cx)
                });
                let work = match step {
                    None | Some(SearchStep::Stale) => return,
                    Some(SearchStep::Done) => break,
                    Some(SearchStep::Read(work)) => work,
                };
                let result = work.await;
                let more = with(&this, cx, |model, cx| {
                    model.apply_search_chunk(generation, result, cx)
                });
                if more != Some(true) {
                    return;
                }
                chunk = (chunk * 2).min(SEARCH_SCAN_CHUNK);
            }
            with(&this, cx, |model, cx| {
                if generation == model.generations.search {
                    model.is_searching = false;
                    Self::changed(BrowserEvent::SearchChanged, cx);
                }
            });
        })
    }

    fn next_search_chunk(
        &mut self,
        generation: u64,
        target: usize,
        chunk: usize,
        cx: &mut Context<Self>,
    ) -> SearchStep {
        if generation != self.generations.search {
            return SearchStep::Stale;
        }
        let (Some(repository), Some(tip), Some(filter)) = (
            self.repository.clone(),
            self.history_anchor,
            self.search_filter.clone(),
        ) else {
            return SearchStep::Stale;
        };
        if self.search_results.len() >= target || self.search_complete {
            return SearchStep::Done;
        }
        let pager = self.search_pager.clone();
        let request = PageRequest {
            start: self.search_scanned,
            limit: target - self.search_results.len(),
            max_scan: chunk,
        };
        SearchStep::Read(blocking(cx.background_executor(), move || {
            HistoryPager::page(pager, &*repository, tip, filter, request)
        }))
    }

    /// Applies one chunk; `true` while the walk goes on.
    fn apply_search_chunk(
        &mut self,
        generation: u64,
        result: ReadResult<(Arc<HistoryPager>, CommitPage)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generations.search {
            return false;
        }
        match result {
            Ok((pager, page)) => {
                self.search_pager = Some(pager);
                self.search_results.extend(page.commits);
                self.search_scanned += page.scanned;
                self.search_complete = page.complete;
                Self::changed(BrowserEvent::SearchChanged, cx);
                true
            }
            Err(error) => {
                self.is_searching = false;
                self.search_error = Some(error.message().to_owned());
                Self::changed(BrowserEvent::SearchChanged, cx);
                Self::fail(error.message(), cx);
                false
            }
        }
    }
}
