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

/// The sidebar's saved width and collapse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedSidebar {
    /// The width it reopens at.
    pub width: f64,
    /// Whether it was collapsed.
    pub collapsed: bool,
}

/// The sidebar as the main split saved it: its width, clamped to
/// [`SIDEBAR_MINIMUM_WIDTH`]..[`SIDEBAR_MAXIMUM_WIDTH`], and whether it was collapsed. `None`
/// when nothing usable is saved, so the default width stands.
#[must_use]
pub fn saved_sidebar(frames: &[SubviewFrame]) -> Option<SavedSidebar> {
    let sidebar = frames.first()?;
    let width = sidebar.frame.width;
    if !width.is_finite() {
        return None;
    }
    Some(SavedSidebar {
        width: width.clamp(SIDEBAR_MINIMUM_WIDTH, SIDEBAR_MAXIMUM_WIDTH),
        collapsed: sidebar.hidden,
    })
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
