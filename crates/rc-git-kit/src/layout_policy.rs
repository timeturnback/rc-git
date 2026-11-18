//! Port of `LayoutPolicy.swift`: how the panes are arranged for a window size, and the
//! sidebar's collapse and fit bookkeeping. Pure, so every threshold is tested without a window.
//!
//! Sizes are plain `f64` points. The sidebar keeps the navigation for whichever mode is
//! showing and the content area beside it is the detail, so only the sidebar changes with
//! window size.

/// How the panes are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Whether the sidebar has room to stay open.
    pub shows_sidebar: bool,
}

/// Below this window width the sidebar collapses: the sidebar and the detail pane no longer
/// both keep a usable width.
pub const SIDEBAR_WINDOW_WIDTH: f64 = 560.0;
/// The narrowest the sidebar goes, however short its rows are. The commit row sets the floor.
pub const SIDEBAR_MINIMUM_WIDTH: f64 = 260.0;
/// The widest the sidebar goes, however long its rows are. Past this the detail pane matters
/// more than a path read in full.
pub const SIDEBAR_MAXIMUM_WIDTH: f64 = 460.0;
/// The narrowest the detail pane beside the sidebar goes. The sidebar threshold leaves room
/// for both floors and the divider between them.
pub const CONTENT_MINIMUM_WIDTH: f64 = 240.0;
/// The smallest window width: the detail pane alone, with the sidebar collapsed, still usable.
pub const WINDOW_MINIMUM_WIDTH: f64 = 440.0;
/// The smallest window height.
pub const WINDOW_MINIMUM_HEIGHT: f64 = 320.0;
/// The height the commit list on top of History's left column never drops below. A pane at
/// zero reads as a missing feature.
pub const HISTORY_COMMITS_FLOOR: f64 = 120.0;
/// The height the open commit's detail, header and changed files, never drops below.
pub const HISTORY_DETAIL_FLOOR: f64 = 200.0;
/// The share of the left column a commit detail opens at when it has no height of its own.
pub const HISTORY_DETAIL_SHARE: f64 = 0.45;
/// The width History's left column (commit list over the detail) keeps while a commit is open.
pub const HISTORY_LIST_MIN_WIDTH: f64 = 320.0;
/// The width the diff of the picked file beside History's left column keeps.
pub const HISTORY_DIFF_MIN_WIDTH: f64 = 340.0;
/// The share of History's width the diff pane opens at when it has no width of its own.
pub const HISTORY_DIFF_SHARE: f64 = 0.5;
/// The narrowest a drag leaves the History commit list's author column.
pub const HISTORY_AUTHOR_MIN_WIDTH: f64 = 48.0;
/// The History author column's width until a drag sizes it: names are short, so it gives way
/// to the message.
pub const HISTORY_AUTHOR_WIDTH: f64 = 100.0;
/// The narrowest a drag leaves the History commit list's date column; below the fitted width
/// the date ends in an ellipsis.
pub const HISTORY_DATE_MIN_WIDTH: f64 = 64.0;
/// The width the History message column keeps when a drag widens the author or date column.
pub const HISTORY_MESSAGE_MIN_WIDTH: f64 = 120.0;
/// The height the Changes tab's file list keeps while the sidebar has room for it. It yields
/// before the commit box does, since it shares the sidebar with the ref list.
pub const CHANGES_FILES_FLOOR: f64 = 150.0;
/// The fixed height of the Changes tab's stash list.
pub const CHANGES_STASH_HEIGHT: f64 = 96.0;
/// The fixed height of the Changes tab's commit message box.
pub const CHANGES_MESSAGE_HEIGHT: f64 = 64.0;
/// The height the Changes tab's diff never drops below.
pub const CHANGES_DIFF_FLOOR: f64 = 150.0;

/// A window or pane size in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    /// Horizontal extent.
    pub width: f64,
    /// Vertical extent.
    pub height: f64,
}

/// The layout for a window of `size`. Only the width decides: a portrait window follows the
/// same sidebar rule.
#[must_use]
pub fn layout(size: Size) -> Layout {
    Layout {
        shows_sidebar: size.width >= SIDEBAR_WINDOW_WIDTH,
    }
}

/// The sidebar width that shows rows `content_width` wide, clamped so the detail pane keeps
/// the rest of the window.
#[must_use]
pub fn sidebar_width(content_width: f64) -> f64 {
    content_width.clamp(SIDEBAR_MINIMUM_WIDTH, SIDEBAR_MAXIMUM_WIDTH)
}

/// The first pane's size along a split's axis, out of the `available` space the two panes
/// share, the divider already taken off.
///
/// `preferred` is where the divider sits now, kept whenever both floors still fit. Space too
/// small for both floors is split between them in proportion, so neither pane reaches zero
/// and the divider stays where a drag can reach it.
#[must_use]
pub fn split_pane(available: f64, preferred: f64, first_floor: f64, second_floor: f64) -> f64 {
    if available <= 0.0 {
        return 0.0;
    }
    if available < first_floor + second_floor {
        return available * first_floor / (first_floor + second_floor);
    }
    preferred.max(first_floor).min(available - second_floor)
}

/// The commit list's height in History's left column, out of the `available` height it shares
/// with the commit detail under it.
///
/// `detail_height` is the detail's height now, kept whenever both floors allow it. A detail
/// below its floor has never been sized (it was hidden until a commit opened), so it opens at
/// [`HISTORY_DETAIL_SHARE`] of the column instead of squeezed onto the floor.
#[must_use]
pub fn history_commits_height(available: f64, detail_height: f64) -> f64 {
    let detail = if detail_height >= HISTORY_DETAIL_FLOOR {
        detail_height
    } else {
        available * HISTORY_DETAIL_SHARE
    };
    split_pane(
        available,
        available - detail,
        HISTORY_COMMITS_FLOOR,
        HISTORY_DETAIL_FLOOR,
    )
}

/// The left column's width in History's outer split, out of the `available` width it shares
/// with the diff pane beside it.
///
/// `diff_width` is the diff pane's width now, kept whenever both floors allow it, so a window
/// resize moves the column and leaves the diff alone. A diff below its floor has never been
/// sized, so it opens at [`HISTORY_DIFF_SHARE`] of the width instead of squeezed onto the
/// floor.
#[must_use]
pub fn history_list_width(available: f64, diff_width: f64) -> f64 {
    let diff = if diff_width >= HISTORY_DIFF_MIN_WIDTH {
        diff_width
    } else {
        available * HISTORY_DIFF_SHARE
    };
    split_pane(
        available,
        available - diff,
        HISTORY_LIST_MIN_WIDTH,
        HISTORY_DIFF_MIN_WIDTH,
    )
}

/// A History commit list column a drag can size. The message column takes what they leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryColumn {
    /// The author's name.
    Author,
    /// The commit's date and time.
    Date,
}

/// The History column widths a drag has set; a column never dragged has none.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SavedHistoryColumns {
    /// The author column's dragged width.
    pub author: Option<f64>,
    /// The date column's dragged width.
    pub date: Option<f64>,
}

/// The widths the History author and date columns are drawn at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryColumns {
    /// The author column's width.
    pub author: f64,
    /// The date column's width.
    pub date: f64,
}

impl HistoryColumns {
    /// `column`'s width.
    #[must_use]
    pub fn width(&self, column: HistoryColumn) -> f64 {
        match column {
            HistoryColumn::Author => self.author,
            HistoryColumn::Date => self.date,
        }
    }
}

/// The History author and date columns' widths: each as `saved` by a drag, else its default,
/// never under its floor.
///
/// The author column defaults to [`HISTORY_AUTHOR_WIDTH`]. The date column defaults to
/// `date_fit`, the widest date the list can show, so it is no wider than its text and the
/// message gets the rest.
#[must_use]
pub fn history_columns(saved: SavedHistoryColumns, date_fit: f64) -> HistoryColumns {
    HistoryColumns {
        author: saved
            .author
            .unwrap_or(HISTORY_AUTHOR_WIDTH)
            .max(HISTORY_AUTHOR_MIN_WIDTH),
        date: saved.date.unwrap_or(date_fit).max(HISTORY_DATE_MIN_WIDTH),
    }
}

/// The width `column` takes when a drag asks for `width`, out of the `room` the message,
/// author and date columns share.
///
/// The other column keeps its width and the message keeps [`HISTORY_MESSAGE_MIN_WIDTH`]; the
/// dragged column never goes under its own floor, even when the room is too small for all
/// three.
#[must_use]
pub fn dragged_history_column(
    columns: HistoryColumns,
    column: HistoryColumn,
    width: f64,
    room: f64,
) -> f64 {
    let (floor, other) = match column {
        HistoryColumn::Author => (HISTORY_AUTHOR_MIN_WIDTH, columns.date),
        HistoryColumn::Date => (HISTORY_DATE_MIN_WIDTH, columns.author),
    };
    let ceiling = room - HISTORY_MESSAGE_MIN_WIDTH - other;
    width.min(ceiling).max(floor)
}

/// Whether the window collapsed the sidebar itself, so that widening the window re-opens only
/// a sidebar the policy closed and never one the user closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarCollapse {
    collapsed_by_policy: bool,
}

impl SidebarCollapse {
    /// A sidebar the policy has not touched.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the current collapse is the policy's rather than the user's.
    #[must_use]
    pub fn collapsed_by_policy(&self) -> bool {
        self.collapsed_by_policy
    }

    /// The collapsed state the sidebar should take for `layout`, or `None` to leave it as it
    /// is.
    ///
    /// A window too narrow for the sidebar collapses an open one and remembers doing so; a
    /// window widening past the threshold re-opens it only when that collapse is still the
    /// policy's.
    pub fn apply(
        &mut self,
        layout: Layout,
        previous: Option<Layout>,
        is_collapsed: bool,
    ) -> Option<bool> {
        if !layout.shows_sidebar && !is_collapsed {
            self.collapsed_by_policy = true;
            return Some(true);
        }
        let widened = previous.is_some_and(|previous| !previous.shows_sidebar);
        if layout.shows_sidebar && widened && self.collapsed_by_policy {
            self.collapsed_by_policy = false;
            return Some(false);
        }
        None
    }

    /// The collapsed state the sidebar should take when a repository opens in the window, or
    /// `None` to leave it as it is. A closed sidebar opens, whoever closed it. In a window too
    /// narrow for it (`current`), it stays closed and the collapse becomes the policy's, so
    /// widening re-opens it.
    pub fn repository_opened(
        &mut self,
        current: Option<Layout>,
        is_collapsed: bool,
    ) -> Option<bool> {
        if !is_collapsed {
            return None;
        }
        let narrow = current.is_some_and(|current| !current.shows_sidebar);
        self.collapsed_by_policy = narrow;
        (!narrow).then_some(false)
    }

    /// The user opened or closed the sidebar by hand: whatever state it is in now is theirs.
    pub fn user_toggled(&mut self) {
        self.collapsed_by_policy = false;
    }
}

/// Whether the sidebar sizes itself to the tree's rows, and when that turns back on. The tree
/// measures nothing while the fit stands back, so a width skipped then must be measured again
/// the moment Files shows or the sidebar re-opens, or the sidebar keeps its stale width.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarFit {
    applies: bool,
}

impl SidebarFit {
    /// A fit that has not applied yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the fit applied at the last update.
    #[must_use]
    pub fn is_applied(&self) -> bool {
        self.applies
    }

    /// Whether the fit applies: a divider the user dragged is theirs, only Files shows the
    /// tree, and a width for a collapsed sidebar would re-open it behind the user.
    #[must_use]
    pub fn applies(is_pinned: bool, shows_files: bool, is_collapsed: bool) -> bool {
        !is_pinned && shows_files && !is_collapsed
    }

    /// Records the fit's state now; `true` when it has just turned on and the tree must
    /// measure its rows again.
    pub fn update(&mut self, is_pinned: bool, shows_files: bool, is_collapsed: bool) -> bool {
        let now = Self::applies(is_pinned, shows_files, is_collapsed);
        let turned_on = now && !self.applies;
        self.applies = now;
        turned_on
    }
}

/// Whether a split view resize was the user dragging a divider, given the user-resize value
/// the resize carried (`None` when it carried none). A programmatic divider move reports 0
/// and a window resize reports nothing, so only a drag counts: the automatic fit and a frame
/// restore must not pin the width.
#[must_use]
pub fn is_user_drag(user_resize: Option<i64>) -> bool {
    user_resize == Some(1)
}
