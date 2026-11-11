//! Port of `Navigator.swift`: decides what the app-wide back/forward list records. Only the
//! front tab's moves are the user's: a background tab reports too (a repository attaching at
//! launch, a ref refresh) and a tab showing a restored stop reports the restore itself.
//! Neither is a visit.
//!
//! A tab is named by an identity the caller picks, such as its window's id. A tab that has no
//! repository yet still has one, so its first stop counts once it is in front.

use crate::back_forward::{BackForwardList, Direction, NavigationStop};

/// The one app-wide back/forward list and the rules for what it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigator<Tab> {
    list: BackForwardList,
    /// The tab in the main window, or `None` before any tab came to the front.
    front: Option<Tab>,
    /// How many stops are being restored.
    pending_restores: usize,
}

impl<Tab> Default for Navigator<Tab> {
    fn default() -> Self {
        Self {
            list: BackForwardList::new(),
            front: None,
            pending_restores: 0,
        }
    }
}

impl<Tab: PartialEq + Clone> Navigator<Tab> {
    /// An empty list with no tab in front.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The stops recorded so far.
    #[must_use]
    pub fn list(&self) -> &BackForwardList {
        &self.list
    }

    /// Whether a step in `direction` has a stop to land on.
    #[must_use]
    pub fn can_go(&self, direction: Direction) -> bool {
        self.list.can_go(direction)
    }

    /// Records `stop` as a visit when `tab` is in front and no restore is running. `true` when
    /// it was recorded, which is when the list may have changed.
    pub fn report(&mut self, stop: NavigationStop, tab: &Tab) -> bool {
        if self.pending_restores > 0 || self.front.as_ref() != Some(tab) {
            return false;
        }
        self.list.visit(stop);
        true
    }

    /// A switch to another tab is a move to where that tab is, `stop`. The same tab becoming
    /// main again, when the app is reactivated, is not. `true` when a stop was recorded.
    pub fn tab_came_to_front(&mut self, tab: &Tab, stop: Option<NavigationStop>) -> bool {
        if self.front.as_ref() == Some(tab) {
            return false;
        }
        self.front = Some(tab.clone());
        let Some(stop) = stop else {
            return false;
        };
        self.report(stop, tab)
    }

    /// Steps the mark one stop and answers the stop to show, or `None` at either end. The
    /// caller shows it in its tab, bracketed by [`Self::begin_restore`] and
    /// [`Self::end_restore`].
    pub fn go(&mut self, direction: Direction) -> Option<NavigationStop> {
        self.list.go(direction).cloned()
    }

    /// `tab` is coming to the front to show a stop: what it reports until
    /// [`Self::end_restore`] is the restore, not a visit.
    pub fn begin_restore(&mut self, tab: &Tab) {
        self.front = Some(tab.clone());
        self.pending_restores += 1;
    }

    /// The restore [`Self::begin_restore`] started has landed.
    pub fn end_restore(&mut self) {
        self.pending_restores = self.pending_restores.saturating_sub(1);
    }

    /// Forgets a closed tab: its stops, and that it was in front. Pass the repository it
    /// showed, or `None` for a tab that never showed one.
    pub fn tab_closed(&mut self, tab: &Tab, repository_path: Option<&str>) {
        if self.front.as_ref() == Some(tab) {
            self.front = None;
        }
        if let Some(repository_path) = repository_path {
            self.list.remove_stops(repository_path);
        }
    }
}
