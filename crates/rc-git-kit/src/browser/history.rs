//! The history pane: pages of the anchor's log, the lane graph beside them, and the scope to
//! one path's commits. Pages are cached per tip, and per tip and path, so switching away and
//! back does not re-walk.

use std::sync::Arc;

use gpui::{Context, Task};
use rc_git_core::{CommitFilter, CommitPage, CommitSummary, Oid};

use super::{
    BrowserEvent, BrowserModel, HISTORY_PAGE_SIZE, HistoryPages, PATH_HISTORY_SCAN_CHUNK,
    PathHistoryKey, PathPages, with,
};
use crate::back_forward::HistoryScope;
use crate::blocking::blocking;
use crate::commit_graph::{GraphBuilder, GraphRow};
use crate::history_paging::{HistoryPager, LogPager, PageRequest};
use crate::models::path_refusal;
use crate::repository::ReadResult;

/// What one round of a path walk does next.
enum PathStep {
    /// The scope moved on; touch nothing.
    Stale,
    /// The page is full or the walk ended.
    Done,
    /// Read the next chunk.
    Read(Task<ReadResult<(Arc<HistoryPager>, CommitPage)>>),
}

impl BrowserModel {
    /// Fetches the next page of commits for the history pane. Does nothing while a page is
    /// loading, after the last page, after a failed page until [`Self::retry_history`], or
    /// when nothing is selected.
    pub fn load_more_history(&mut self, cx: &mut Context<Self>) -> Task<()> {
        match self.history_scope.clone() {
            HistoryScope::All => self.load_more_log(cx),
            HistoryScope::Path(path) => self.load_more_path_history(path, cx),
        }
    }

    /// Clears a failed history page and asks for it again. Does nothing without a failure.
    pub fn retry_history(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.history_error.is_none() {
            return Task::ready(());
        }
        self.history_error = None;
        Self::changed(BrowserEvent::HistoryChanged, cx);
        self.load_more_history(cx)
    }

    fn can_page(&self) -> bool {
        !self.is_loading_history && !self.history_complete && self.history_error.is_none()
    }

    fn load_more_log(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let (Some(repository), Some(tip)) = (self.repository.clone(), self.history_anchor) else {
            return Task::ready(());
        };
        if !self.can_page() {
            return Task::ready(());
        }
        self.is_loading_history = true;
        let generation = self.generations.history;
        let start = self.history.len();
        let pager = self
            .history_cache
            .value(&tip)
            .and_then(|pages| pages.pager.clone());
        let work = blocking(cx.background_executor(), move || {
            LogPager::page(pager, &*repository, tip, start, HISTORY_PAGE_SIZE)
        });
        Self::run(cx, (), async move |this, cx| {
            let result = work.await;
            with(&this, cx, |model, cx| {
                model.apply_log_page(generation, tip, result, cx);
            });
        })
    }

    fn apply_log_page(
        &mut self,
        generation: u64,
        tip: Oid,
        result: ReadResult<(Arc<LogPager>, Vec<CommitSummary>)>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generations.history {
            return;
        }
        self.is_loading_history = false;
        match result {
            Ok((pager, page)) => {
                // Released first, so the appends extend the loaded pages in place rather
                // than copying everything the cache shares with them.
                self.history_cache.remove(&tip);
                self.history_complete = page.len() < HISTORY_PAGE_SIZE;
                Arc::make_mut(&mut self.graph).append(&page);
                Arc::make_mut(&mut self.history).extend(page);
                let pages = HistoryPages {
                    commits: Arc::clone(&self.history),
                    complete: self.history_complete,
                    graph: Arc::clone(&self.graph),
                    pager: Some(pager),
                };
                self.history_cache.insert(tip, pages);
                Self::changed(BrowserEvent::HistoryChanged, cx);
            }
            Err(error) => {
                self.history_error = Some(error.message().to_owned());
                Self::changed(BrowserEvent::HistoryChanged, cx);
                Self::fail(error.message(), cx);
            }
        }
    }

    /// The lane graph rows aligned with the history. Empty while a path scope or search shows
    /// a filtered list, whose rows have no meaningful topology.
    #[must_use]
    pub fn graph_rows(&self) -> &[GraphRow] {
        if self.history_scope != HistoryScope::All || self.search_filter.is_some() {
            return &[];
        }
        self.graph.rows()
    }

    /// The widest the current graph has been, for sizing the graph column.
    #[must_use]
    pub fn graph_max_lane_count(&self) -> usize {
        self.graph.max_lane_count()
    }

    /// One page of path-scoped history, walked in scan chunks so a long stretch without a
    /// match never blocks a thread for long and a scope change between chunks drops the rest.
    fn load_more_path_history(&mut self, path: String, cx: &mut Context<Self>) -> Task<()> {
        let (Some(_), Some(tip)) = (self.repository.as_ref(), self.history_anchor) else {
            return Task::ready(());
        };
        if !self.can_page() {
            return Task::ready(());
        }
        self.is_loading_history = true;
        let generation = self.generations.history;
        let key = PathHistoryKey { tip, path };
        let target = self.history.len() + HISTORY_PAGE_SIZE;
        Self::run(cx, (), async move |this, cx| {
            loop {
                let step = with(&this, cx, |model, cx| {
                    model.next_path_chunk(generation, &key, target, cx)
                });
                let work = match step {
                    None | Some(PathStep::Stale) => return,
                    Some(PathStep::Done) => break,
                    Some(PathStep::Read(work)) => work,
                };
                let result = work.await;
                let more = with(&this, cx, |model, cx| {
                    model.apply_path_chunk(generation, &key, result, cx)
                });
                if more != Some(true) {
                    return;
                }
            }
            with(&this, cx, |model, _| model.is_loading_history = false);
        })
    }

    fn next_path_chunk(
        &mut self,
        generation: u64,
        key: &PathHistoryKey,
        target: usize,
        cx: &mut Context<Self>,
    ) -> PathStep {
        if generation != self.generations.history {
            return PathStep::Stale;
        }
        let Some(repository) = self.repository.clone() else {
            return PathStep::Stale;
        };
        if self.history.len() >= target || self.history_complete {
            return PathStep::Done;
        }
        let start = self
            .path_history_cache
            .value(key)
            .map_or(0, |pages| pages.scanned);
        let remaining = target - self.history.len();
        let pager = self
            .path_pager
            .as_ref()
            .filter(|(open, open_generation, _)| open == key && *open_generation == generation)
            .map(|(_, _, pager)| Arc::clone(pager));
        let tip = key.tip;
        let filter = CommitFilter {
            path: Some(key.path.clone()),
            ..CommitFilter::default()
        };
        let request = PageRequest {
            start,
            limit: remaining,
            max_scan: PATH_HISTORY_SCAN_CHUNK,
        };
        PathStep::Read(blocking(cx.background_executor(), move || {
            HistoryPager::page(pager, &*repository, tip, filter, request)
        }))
    }

    /// Applies one chunk; `true` while the walk goes on.
    fn apply_path_chunk(
        &mut self,
        generation: u64,
        key: &PathHistoryKey,
        result: ReadResult<(Arc<HistoryPager>, CommitPage)>,
        cx: &mut Context<Self>,
    ) -> bool {
        if generation != self.generations.history {
            return false;
        }
        match result {
            Ok((pager, page)) => {
                let start = self
                    .path_history_cache
                    .remove(key)
                    .map_or(0, |pages| pages.scanned);
                self.path_pager = Some((key.clone(), generation, pager));
                Arc::make_mut(&mut self.history).extend(page.commits);
                self.history_complete = page.complete;
                let pages = PathPages {
                    commits: Arc::clone(&self.history),
                    scanned: start + page.scanned,
                    complete: page.complete,
                };
                self.path_history_cache.insert(key.clone(), pages);
                Self::changed(BrowserEvent::HistoryChanged, cx);
                true
            }
            Err(error) => {
                // Keep the commits already shown; the error stops paging. Not cached:
                // scoping again or a retry walks again.
                self.is_loading_history = false;
                self.history_error = Some(error.message().to_owned());
                Self::changed(BrowserEvent::HistoryChanged, cx);
                Self::fail(error.message(), cx);
                false
            }
        }
    }

    /// Scopes the history pane to the commits that changed `path`, keeping the anchor. Cached
    /// pages for the same tip and path come back without a walk. A path whose `path_is_exact`
    /// is false is refused through [`BrowserEvent::Error`]: the walk matches by path, so it
    /// would show another file's history.
    pub fn show_path_history(
        &mut self,
        path: &str,
        path_is_exact: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        if !path_is_exact {
            Self::fail(path_refusal("show the history of", path), cx);
            return Task::ready(());
        }
        if !self.scope_history(path, cx) || self.history_complete {
            return Task::ready(());
        }
        self.load_more_history(cx)
    }

    /// The half of [`Self::show_path_history`] that reads nothing: the scope changes and the
    /// cached pages show. `false` when there is no history to scope.
    pub(super) fn scope_history(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        let (Some(_), Some(tip)) = (self.repository.as_ref(), self.history_anchor) else {
            return false;
        };
        self.generations.history += 1;
        self.path_pager = None;
        self.is_loading_history = false;
        self.history_error = None;
        self.history_scope = HistoryScope::Path(path.to_owned());
        self.clear_search(cx);
        self.clear_commit_detail(cx);
        let key = PathHistoryKey {
            tip,
            path: path.to_owned(),
        };
        if let Some(cached) = self.path_history_cache.value(&key) {
            self.history = Arc::clone(&cached.commits);
            self.history_complete = cached.complete;
        } else {
            self.history = Arc::default();
            self.history_complete = false;
        }
        Self::changed(BrowserEvent::HistoryChanged, cx);
        self.note_location(cx);
        true
    }

    /// The text Copy Path puts on the pasteboard, or `None` after refusing an inexact path
    /// through [`BrowserEvent::Error`]: its U+FFFD spelling names a different file, or none.
    pub fn copyable_path(
        &mut self,
        path: &str,
        path_is_exact: bool,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if !path_is_exact {
            Self::fail(path_refusal("copy the path of", path), cx);
            return None;
        }
        Some(path.to_owned())
    }

    /// Returns the history pane to the unfiltered list of the current anchor.
    pub fn clear_history_scope(&mut self, cx: &mut Context<Self>) {
        if self.history_scope == HistoryScope::All {
            return;
        }
        self.reset_history(self.history_anchor, cx);
        self.note_location(cx);
    }

    /// Starts the history over at `anchor`: the plain list, from the cache when the tip was
    /// walked before. A search waiting for an anchor keeps waiting.
    pub(super) fn reset_history(&mut self, anchor: Option<Oid>, cx: &mut Context<Self>) {
        self.generations.history += 1;
        self.path_pager = None;
        self.is_loading_history = false;
        self.history_error = None;
        self.history_anchor = anchor;
        self.history_scope = HistoryScope::All;
        let waiting = self.pending_search.take();
        self.clear_search(cx);
        self.pending_search = waiting;
        self.clear_commit_detail(cx);
        let cached = anchor.and_then(|anchor| self.history_cache.value(&anchor).cloned());
        if let Some(cached) = cached {
            self.history = cached.commits;
            self.history_complete = cached.complete;
            self.graph = cached.graph;
        } else {
            self.history = Arc::default();
            self.history_complete = false;
            self.graph = Arc::new(GraphBuilder::new());
        }
        Self::changed(BrowserEvent::HistoryChanged, cx);
    }
}
