//! History's two splits: the commit list over the open commit's detail
//! (`rc-git.history-column.v2`) and, in the detail's Changes tab, the changed-file list beside
//! the diff (`rc-git.history.v6`). The second pane of each opens at the size saved under those
//! keys, through `LayoutPolicy`, which keeps a saved size only while both floors fit and opens
//! a never-sized pane at its share; a drag saves the new size. Floors bound a drag through each
//! panel's size range.

use std::ops::Range;

use gpui::{App, AppContext, Context, Entity, Pixels, Subscription, Window};
use gpui_component::resizable::{ResizablePanelEvent, ResizableState};
use rc_git_kit::layout_policy::{
    HISTORY_COMMITS_FLOOR, HISTORY_DETAIL_FLOOR, HISTORY_DIFF_MIN_WIDTH, HISTORY_FILES_MIN_WIDTH,
    history_commits_height, history_files_width,
};
use rc_git_kit::platform::keys::{HISTORY_COLUMN_SPLIT, HISTORY_SPLIT};
use rc_git_kit::platform::stores::{SharedSettings, SubviewFrame};
use rc_git_kit::window_geometry::{SplitAxis, THIN_DIVIDER, saved_second_pane, split_frames};

use crate::log;
use crate::style::{points, value};

/// One saved split.
struct Split {
    name: &'static str,
    axis: SplitAxis,
    state: Entity<ResizableState>,
    /// The second pane's size as last saved or dragged.
    second: Option<f64>,
}

impl Split {
    fn load(name: &'static str, axis: SplitAxis, settings: &SharedSettings, cx: &mut App) -> Self {
        let second = SubviewFrame::load_split(&**settings, name)
            .as_deref()
            .and_then(|frames| saved_second_pane(frames, axis));
        Self {
            name,
            axis,
            state: cx.new(|_| ResizableState::default()),
            second,
        }
    }

    fn available(&self, fallback: Pixels, cx: &App) -> f64 {
        let measured = value(self.state.read(cx).container_size());
        let available = if measured > 0.0 {
            measured
        } else {
            value(fallback)
        };
        (available - THIN_DIVIDER).max(0.0)
    }

    fn resized(&mut self, settings: &SharedSettings, cx: &App) {
        let sizes = self.state.read(cx).sizes().clone();
        let (Some(&first), Some(&second)) = (sizes.first(), sizes.get(1)) else {
            return;
        };
        let (first, second) = (value(first), value(second));
        self.second = Some(second);
        let frames = split_frames(self.axis, first, second, 0.0);
        if let Err(error) = SubviewFrame::save_split(&**settings, self.name, &frames) {
            log::error(&error.to_string());
        }
    }
}

/// Both History splits of one pane.
pub struct HistorySplits {
    settings: SharedSettings,
    diff: Split,
    column: Split,
}

impl HistorySplits {
    /// The splits as last saved.
    pub fn load(settings: SharedSettings, cx: &mut App) -> Self {
        let diff = Split::load(HISTORY_SPLIT, SplitAxis::SideBySide, &settings, cx);
        let column = Split::load(HISTORY_COLUMN_SPLIT, SplitAxis::Stacked, &settings, cx);
        Self {
            settings,
            diff,
            column,
        }
    }

    /// Saves each split when a drag ends.
    pub fn watch<T: 'static>(
        &self,
        splits: impl Fn(&mut T) -> &mut Self + Clone + 'static,
        cx: &mut Context<T>,
    ) -> Vec<Subscription> {
        let diff = splits.clone();
        vec![
            cx.subscribe(
                &self.diff.state,
                move |this, _, _: &ResizablePanelEvent, cx| {
                    let splits = diff(this);
                    splits.diff.resized(&splits.settings, cx);
                },
            ),
            cx.subscribe(
                &self.column.state,
                move |this, _, _: &ResizablePanelEvent, cx| {
                    let splits = splits(this);
                    splits.column.resized(&splits.settings, cx);
                },
            ),
        ]
    }

    /// The second pane sizes as last saved or dragged: the diff's width, the detail's height.
    #[cfg(test)]
    pub(crate) fn saved(&self) -> (Option<f64>, Option<f64>) {
        (self.diff.second, self.column.second)
    }

    /// The file list | diff split's state.
    pub fn diff(&self) -> &Entity<ResizableState> {
        &self.diff.state
    }

    /// The list over detail split's state.
    pub fn column(&self) -> &Entity<ResizableState> {
        &self.column.state
    }

    /// The width the diff opens at, out of the Changes tab's.
    pub fn diff_width(&self, window: &Window, cx: &App) -> Pixels {
        let available = self.diff.available(window.viewport_size().width, cx);
        let diff = self.diff.second.unwrap_or(0.0);
        points(available - history_files_width(available, diff))
    }

    /// The height the detail opens at, out of the pane's.
    pub fn detail_height(&self, window: &Window, cx: &App) -> Pixels {
        let available = self.column.available(window.viewport_size().height, cx);
        let detail = self.column.second.unwrap_or(0.0);
        points(available - history_commits_height(available, detail))
    }
}

/// The range a drag may move the changed-file list's width through.
#[must_use]
pub fn files_range() -> Range<Pixels> {
    points(HISTORY_FILES_MIN_WIDTH)..Pixels::MAX
}

/// The range a drag may move the diff's width through.
#[must_use]
pub fn diff_range() -> Range<Pixels> {
    points(HISTORY_DIFF_MIN_WIDTH)..Pixels::MAX
}

/// The range a drag may move the commit list's height through.
#[must_use]
pub fn commits_range() -> Range<Pixels> {
    points(HISTORY_COMMITS_FLOOR)..Pixels::MAX
}

/// The range a drag may move the detail's height through.
#[must_use]
pub fn detail_range() -> Range<Pixels> {
    points(HISTORY_DETAIL_FLOOR)..Pixels::MAX
}
