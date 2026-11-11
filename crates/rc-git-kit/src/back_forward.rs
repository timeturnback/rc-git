//! Port of `BackForwardList.swift`: the stops the user went through, across every tab, with
//! the one on screen marked. It works the way a browser's does: a move somewhere new drops the
//! stops ahead, and stepping back or forward only moves the mark.
//!
//! This module also owns the location types a stop names ([`ContentMode`], [`BrowsedTree`],
//! [`HistoryScope`], [`BrowserLocation`], [`NavigationStop`]); the browser model and the
//! navigator import them from here.

use rc_git_core::Oid;

/// Which pane fills the content area beside the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentMode {
    /// The file tree and the open file.
    Files,
    /// The commit list, a commit's detail and its diff.
    History,
    /// The working tree's changes and the commit box.
    Changes,
}

/// What a tab's tree pane browses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowsedTree {
    /// No repository, or one with no ref to select.
    #[default]
    Nothing,
    /// The files on disk.
    WorkingTree,
    /// A ref by its full name, so returning to it follows the ref when it has moved.
    Ref(String),
    /// One commit picked from the history.
    Commit {
        /// The commit browsed.
        commit: Oid,
        /// The tip whose history the pane kept showing.
        history_tip: Option<Oid>,
    },
}

/// Which commits the history pane lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HistoryScope {
    /// Every commit reachable from the tip.
    #[default]
    All,
    /// Only the commits that changed this path.
    Path(String),
}

/// Where one tab's browser is: the tree it browses, the file in the file pane, what the
/// history pane lists, and the commit in its detail. It names things rather than holding them,
/// so going back to one reads the repository as it is then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserLocation {
    /// What the tree pane browses.
    pub tree: BrowsedTree,
    /// The file in the file pane.
    pub file_path: Option<String>,
    /// Whether every component of `file_path` is the stored name byte for byte. A path with
    /// U+FFFD replacements names no one file, so returning to it reads nothing.
    pub file_path_is_exact: bool,
    /// What the history pane lists.
    pub history_scope: HistoryScope,
    /// The commit in the history pane's detail.
    pub detail_commit: Option<Oid>,
}

impl Default for BrowserLocation {
    fn default() -> Self {
        Self {
            tree: BrowsedTree::Nothing,
            file_path: None,
            file_path_is_exact: true,
            history_scope: HistoryScope::All,
            detail_commit: None,
        }
    }
}

/// One stop of the back/forward list: a tab, the pane it shows, and where its browser is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationStop {
    /// The repository of the tab, which is what names a tab: a repository opens in one only.
    pub repository_path: String,
    /// The pane the tab shows.
    pub mode: ContentMode,
    /// Where the tab's browser is.
    pub browser: BrowserLocation,
}

/// Which way a step goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// To the stop before the current one.
    Back,
    /// To the stop after the current one.
    Forward,
}

impl Direction {
    /// The fourth and fifth buttons of a mouse, numbered 3 and 4 from zero.
    #[must_use]
    pub fn from_mouse_button(button: i64) -> Option<Self> {
        match button {
            3 => Some(Self::Back),
            4 => Some(Self::Forward),
            _ => None,
        }
    }

    /// A finished swipe, by the amount the gesture tracking reports: positive uncovers the
    /// stop before this one. `None` for a swipe that went nowhere.
    #[must_use]
    pub fn from_swipe(amount: f64) -> Option<Self> {
        if amount > 0.0 {
            Some(Self::Back)
        } else if amount < 0.0 {
            Some(Self::Forward)
        } else {
            None
        }
    }
}

/// How many stops the list keeps; the oldest fall off past it.
pub const LIMIT: usize = 200;

/// The stops the user went through, across every tab, with the one on screen marked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackForwardList {
    stops: Vec<NavigationStop>,
    /// Which of `stops` is on screen. Inside `stops` whenever there are any.
    index: usize,
}

impl BackForwardList {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every stop, oldest first.
    #[must_use]
    pub fn stops(&self) -> &[NavigationStop] {
        &self.stops
    }

    /// The index of the stop on screen; 0 when the list is empty.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// The stop on screen, `None` when the list is empty.
    #[must_use]
    pub fn current(&self) -> Option<&NavigationStop> {
        self.stops.get(self.index)
    }

    /// Whether a step in `direction` has a stop to land on.
    #[must_use]
    pub fn can_go(&self, direction: Direction) -> bool {
        match direction {
            Direction::Back => self.index > 0,
            Direction::Forward => self.index + 1 < self.stops.len(),
        }
    }

    /// Records a move to `stop`. A move to where the list already is records nothing;
    /// anything else drops the stops ahead of the current one.
    pub fn visit(&mut self, stop: NavigationStop) {
        if self.current() == Some(&stop) {
            return;
        }
        self.stops.truncate(self.index + 1);
        self.stops.push(stop);
        if self.stops.len() > LIMIT {
            let excess = self.stops.len() - LIMIT;
            self.stops.drain(..excess);
        }
        self.index = self.stops.len() - 1;
    }

    /// Steps the mark one stop and answers the stop to show, or `None` at either end.
    pub fn go(&mut self, direction: Direction) -> Option<&NavigationStop> {
        if !self.can_go(direction) {
            return None;
        }
        match direction {
            Direction::Back => self.index -= 1,
            Direction::Forward => self.index += 1,
        }
        self.stops.get(self.index)
    }

    /// Drops every stop in one repository, which is what closing its tab does. The mark stays
    /// on the stop it was on, or falls back to the nearest one before it; two stops the
    /// dropped ones kept apart become one when they are the same place.
    pub fn remove_stops(&mut self, repository_path: &str) {
        let mut kept: Vec<NavigationStop> = Vec::new();
        let mut kept_index = 0;
        for (offset, stop) in std::mem::take(&mut self.stops).into_iter().enumerate() {
            if stop.repository_path == repository_path {
                continue;
            }
            if kept.last() != Some(&stop) {
                kept.push(stop);
            }
            if offset <= self.index {
                kept_index = kept.len() - 1;
            }
        }
        self.stops = kept;
        self.index = kept_index;
    }

    /// Whether stepping back shows another commit in the same tab's history pane, which is
    /// when the commit detail offers its own back arrow.
    #[must_use]
    pub fn back_returns_to_commit(&self, repository_path: &str) -> bool {
        let Some(current) = self.current() else {
            return false;
        };
        let Some(previous) = self
            .index
            .checked_sub(1)
            .and_then(|index| self.stops.get(index))
        else {
            return false;
        };
        current.repository_path == repository_path
            && previous.repository_path == repository_path
            && current.mode == ContentMode::History
            && previous.mode == ContentMode::History
            && previous.browser.detail_commit.is_some()
            && previous.browser.detail_commit != current.browser.detail_commit
    }
}
