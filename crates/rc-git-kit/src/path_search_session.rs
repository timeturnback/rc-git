//! Port of `PathSearchSession.swift`: one Go to File query session. It runs each query, keeps
//! only the newest one's result, and holds a Return pressed while that result is still in
//! flight until it lands, so a fast typist opens the first match of what they typed rather
//! than a row of the older list, or nothing.
//!
//! An older query is not stopped: like the Swift tasks, it runs to its end and the generation
//! counter drops what it brings back. Stopping the scan itself is the provider's job (the
//! browser model cancels a superseded index scan).

use std::rc::Rc;

use gpui::{App, Context, EventEmitter, Task};

use crate::path_search::PathIndexEntry;

/// What a path search found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathSearchResult {
    /// The best matches, best first.
    pub entries: Vec<PathIndexEntry>,
    /// Whether the index stopped at its cap, so matches may be missing.
    pub truncated: bool,
}

/// Why a path search brought nothing back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSearchError {
    /// A newer query superseded it; nothing to show.
    Cancelled,
    /// The index could not be built or read; the message says why.
    Failed(String),
}

/// What runs a query for a session: the browser model's path search, or a test's stand-in.
pub type PathSearchProvider =
    Rc<dyn Fn(String, &mut App) -> Task<Result<PathSearchResult, PathSearchError>>>;

/// What the session tells the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSearchEvent {
    /// The newest query landed; the results are updated.
    Results {
        /// Whether matches may be missing.
        truncated: bool,
    },
    /// The newest query failed; the list is empty.
    Failed(String),
    /// Open this file.
    Open(PathIndexEntry),
}

/// One Go to File panel session.
#[derive(Default)]
pub struct PathSearchSession {
    provider: Option<PathSearchProvider>,
    /// The list on screen. It belongs to the newest query only while no search is in flight.
    results: Vec<PathIndexEntry>,
    search_generation: u64,
    applied_generation: u64,
    open_when_results_land: bool,
}

impl EventEmitter<PathSearchEvent> for PathSearchSession {}

impl PathSearchSession {
    /// A session with no provider; set one with [`Self::set_provider`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets what runs a query.
    pub fn set_provider(&mut self, provider: PathSearchProvider) {
        self.provider = Some(provider);
    }

    /// The list on screen.
    #[must_use]
    pub fn results(&self) -> &[PathIndexEntry] {
        &self.results
    }

    /// Empties the list and drops any pending open and any search still in flight.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.search_generation += 1;
        self.results.clear();
        self.open_when_results_land = false;
        cx.notify();
    }

    /// Drops a Return that is waiting for results, as when the panel closes.
    pub fn cancel_pending_open(&mut self) {
        self.open_when_results_land = false;
    }

    /// Starts `query`; any older query still running is ignored when it lands. `false` when
    /// there is no provider.
    pub fn search(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        let Some(provider) = self.provider.clone() else {
            return false;
        };
        self.search_generation += 1;
        let generation = self.search_generation;
        let search = provider(query.to_owned(), cx);
        cx.spawn(async move |this, cx| {
            let outcome = search.await;
            let _ = this.update(cx, |session, cx| session.land(generation, outcome, cx));
        })
        .detach();
        true
    }

    fn land(
        &mut self,
        generation: u64,
        outcome: Result<PathSearchResult, PathSearchError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.search_generation {
            return;
        }
        match outcome {
            Ok(result) => {
                self.results = result.entries;
                self.applied_generation = generation;
                cx.emit(PathSearchEvent::Results {
                    truncated: result.truncated,
                });
                if self.open_when_results_land {
                    self.open_when_results_land = false;
                    self.open(0, cx);
                }
            }
            Err(PathSearchError::Cancelled) => return,
            Err(PathSearchError::Failed(message)) => {
                self.results.clear();
                self.applied_generation = generation;
                self.open_when_results_land = false;
                cx.emit(PathSearchEvent::Failed(message));
            }
        }
        cx.notify();
    }

    /// Opens `row` of the list on screen, or, while the newest query is still in flight, its
    /// first match once it lands. A row outside the list opens nothing.
    pub fn open(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.applied_generation != self.search_generation {
            self.open_when_results_land = true;
            return;
        }
        if let Some(entry) = self.results.get(row) {
            cx.emit(PathSearchEvent::Open(entry.clone()));
        }
    }
}
