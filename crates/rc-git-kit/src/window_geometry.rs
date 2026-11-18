//! The window frame between the two coordinate systems that hold it: the saved
//! [`WindowFrame`] keeps `AppKit`'s (origin at the bottom-left of the primary screen, y up),
//! and the UI toolkit places windows from the top-left of the primary screen, y down. One
//! conversion each way, so a frame saved by either app restores in the other.

use crate::layout_policy::{
    SIDEBAR_MAXIMUM_WIDTH, SIDEBAR_MINIMUM_WIDTH, WINDOW_MINIMUM_HEIGHT, WINDOW_MINIMUM_WIDTH,
};
use crate::platform::stores::{Rect, SubviewFrame, WindowFrame};

/// The default window size before any frame is saved.
pub const DEFAULT_WIDTH: f64 = 1120.0;
/// See [`DEFAULT_WIDTH`].
pub const DEFAULT_HEIGHT: f64 = 720.0;

/// A rectangle measured from the top-left of the primary screen, y down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopLeftRect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// `rect` in `AppKit`'s coordinates, on a primary screen `primary_height` tall.
#[must_use]
pub fn to_appkit(rect: TopLeftRect, primary_height: f64) -> Rect {
    Rect {
        x: rect.x,
        y: primary_height - rect.y - rect.height,
        width: rect.width,
        height: rect.height,
    }
}

/// `rect`, in `AppKit`'s coordinates, measured from the top-left instead.
#[must_use]
pub fn from_appkit(rect: Rect, primary_height: f64) -> TopLeftRect {
    TopLeftRect {
        x: rect.x,
        y: primary_height - rect.y - rect.height,
        width: rect.width,
        height: rect.height,
    }
}

/// Where a saved frame restores, or `None` when it should not: a frame below the window
/// minimum (another build wrote it, or it is corrupt), or one that meets none of `screens`
/// (saved on a display that is now detached, so it would open fully off screen).
#[must_use]
pub fn restorable(
    saved: &WindowFrame,
    primary_height: f64,
    screens: &[TopLeftRect],
) -> Option<TopLeftRect> {
    let frame = saved.frame;
    if !(frame.width >= WINDOW_MINIMUM_WIDTH && frame.height >= WINDOW_MINIMUM_HEIGHT) {
        return None;
    }
    let rect = from_appkit(frame, primary_height);
    screens
        .iter()
        .any(|screen| intersects(&rect, screen))
        .then_some(rect)
}

fn intersects(a: &TopLeftRect, b: &TopLeftRect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// The sidebar's width as the main split saved it, clamped to
/// [`SIDEBAR_MINIMUM_WIDTH`]..[`SIDEBAR_MAXIMUM_WIDTH`]. `None` when nothing usable is saved,
/// so the default width stands. The saved collapse is not read: a window opens with its
/// sidebar open.
#[must_use]
pub fn saved_sidebar_width(frames: &[SubviewFrame]) -> Option<f64> {
    let width = frames.first()?.frame.width;
    width
        .is_finite()
        .then(|| width.clamp(SIDEBAR_MINIMUM_WIDTH, SIDEBAR_MAXIMUM_WIDTH))
}

/// The two subview frames the main split saves: the sidebar `sidebar_width` wide (kept while
/// collapsed, so it reopens at that width) and the content beside it, both `height` tall in a
/// window `window_width` wide.
#[must_use]
pub fn sidebar_frames(
    sidebar_width: f64,
    collapsed: bool,
    window_width: f64,
    height: f64,
) -> [SubviewFrame; 2] {
    let content_x = if collapsed { 0.0 } else { sidebar_width };
    [
        SubviewFrame {
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: sidebar_width,
                height,
            },
            hidden: collapsed,
            flag: false,
        },
        SubviewFrame {
            frame: Rect {
                x: content_x,
                y: 0.0,
                width: (window_width - content_x).max(0.0),
                height,
            },
            hidden: false,
            flag: false,
        },
    ]
}

/// How a two-pane split lays its panes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    /// Side by side: History's changed-file list and diff (`rc-git.history.v6`).
    SideBySide,
    /// One over the other: History's commit list and detail (`rc-git.history-column.v2`).
    Stacked,
}

/// The thickness of a thin split divider, which `AppKit` counted between the saved frames.
pub const THIN_DIVIDER: f64 = 1.0;

/// The second pane's saved extent along `axis` (the diff's width, the detail's height), or
/// `None` when nothing usable is saved: no second frame, a non-finite or non-positive extent,
/// or a pane `AppKit` saved collapsed. The caller hands it to `LayoutPolicy`, which keeps it
/// only while both floors fit.
#[must_use]
pub fn saved_second_pane(frames: &[SubviewFrame], axis: SplitAxis) -> Option<f64> {
    let second = frames.get(1)?;
    let extent = match axis {
        SplitAxis::SideBySide => second.frame.width,
        SplitAxis::Stacked => second.frame.height,
    };
    (extent.is_finite() && extent > 0.0 && !second.hidden).then_some(extent)
}

/// The two subview frames a split saves: the first pane `first` long along `axis`, a thin
/// divider, then the second `second` long, both `cross` across.
#[must_use]
pub fn split_frames(axis: SplitAxis, first: f64, second: f64, cross: f64) -> [SubviewFrame; 2] {
    let frame = |offset: f64, extent: f64| SubviewFrame {
        frame: match axis {
            SplitAxis::SideBySide => Rect {
                x: offset,
                y: 0.0,
                width: extent,
                height: cross,
            },
            SplitAxis::Stacked => Rect {
                x: 0.0,
                y: offset,
                width: cross,
                height: extent,
            },
        },
        hidden: false,
        flag: false,
    };
    [frame(0.0, first), frame(first + THIN_DIVIDER, second)]
}

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn a_split_round_trips_its_second_pane() {
        for axis in [SplitAxis::SideBySide, SplitAxis::Stacked] {
            let frames = split_frames(axis, 400.0, 360.0, 700.0);
            assert_eq!(saved_second_pane(&frames, axis), Some(360.0), "{axis:?}");
        }
        let side = split_frames(SplitAxis::SideBySide, 400.0, 360.0, 700.0);
        assert_eq!(side[1].frame.x.to_bits(), 401.0_f64.to_bits());
        assert_eq!(side[1].frame.height.to_bits(), 700.0_f64.to_bits());
        let stacked = split_frames(SplitAxis::Stacked, 300.0, 200.0, 500.0);
        assert_eq!(stacked[1].frame.y.to_bits(), 301.0_f64.to_bits());
        assert_eq!(stacked[1].frame.width.to_bits(), 500.0_f64.to_bits());
    }

    #[test]
    fn an_unusable_saved_pane_is_none() {
        let axis = SplitAxis::SideBySide;
        assert_eq!(saved_second_pane(&[], axis), None);
        let one = split_frames(axis, 400.0, 360.0, 700.0);
        assert_eq!(saved_second_pane(&one[..1], axis), None);
        for width in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let mut frames = split_frames(axis, 400.0, 360.0, 700.0);
            frames[1].frame.width = width;
            assert_eq!(saved_second_pane(&frames, axis), None, "{width}");
        }
        let mut hidden = split_frames(axis, 400.0, 360.0, 700.0);
        hidden[1].hidden = true;
        assert_eq!(saved_second_pane(&hidden, axis), None);
    }
}
