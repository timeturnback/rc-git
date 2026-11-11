//! Spacing tokens and the type ramp (`Style.swift`), so the sidebar and the detail pane line up
//! and no view carries a layout number of its own. Colors are not here: every view resolves
//! them from the active theme at render, so the window follows the system appearance.

use gpui::{Pixels, px};

/// The horizontal margin inside the sidebar and in the bars across the top of a pane; a
/// list's selection is inset by the same amount, so a banner above it lines up.
pub const MARGIN: Pixels = px(10.);
/// The space above and below the controls of a bar across the top of a pane.
pub const BAR_PADDING: Pixels = px(8.);
/// The gap between a row's icon and its label, and between neighboring controls.
pub const ROW_GAP: Pixels = px(6.);
/// The gap between groups of toolbar controls.
pub const GROUP_GAP: Pixels = px(16.);
/// How far each folder level of the tree indents.
pub const FOLDER_INDENT: Pixels = px(12.);
/// The size of a row or toolbar icon.
pub const ICON_SIZE: Pixels = px(14.);
/// The height of the app-drawn toolbar row in the transparent titlebar.
pub const TOOLBAR_HEIGHT: Pixels = px(38.);
/// Where the toolbar's controls start, past the window's close, minimize and zoom buttons.
pub const TRAFFIC_LIGHT_INSET: Pixels = px(78.);
/// Where the window buttons sit inside the toolbar row.
pub const TRAFFIC_LIGHT_X: Pixels = px(12.);
/// See [`TRAFFIC_LIGHT_X`].
pub const TRAFFIC_LIGHT_Y: Pixels = px(12.);
/// The thickness of a separator line.
pub const HAIRLINE: Pixels = px(1.);
/// The corner radius of a banner, a find bar, a popover.
pub const RADIUS: Pixels = px(6.);

/// The width of the source viewer's line-number gutter.
pub const GUTTER_WIDTH: Pixels = px(56.);
/// The space between the gutter and the text.
pub const GUTTER_GAP: Pixels = px(8.);
/// The source viewer's font.
pub const MONOSPACE_FONT: &str = "Menlo";
/// The width of the find field in the file pane's find bar.
pub const FIND_FIELD_WIDTH: Pixels = px(240.);
/// The ref picker's size, under the banner.
pub const PICKER_WIDTH: Pixels = px(320.);
/// See [`PICKER_WIDTH`].
pub const PICKER_HEIGHT: Pixels = px(420.);
/// The ref picker's offset under the banner.
pub const PICKER_OFFSET: Pixels = px(4.);

/// Monospaced text: the source viewer and its gutter.
pub const TEXT_MONOSPACE: Pixels = px(12.);
/// Captions: a header's detail, a section heading, a banner's second line.
pub const TEXT_CAPTION: Pixels = px(11.);
/// Body text: rows, labels, buttons.
pub const TEXT_BODY: Pixels = px(13.);

/// The opacity of a selection or find-match background drawn over the text.
pub const SELECTION_OPACITY: f32 = 0.35;
/// The opacity of a non-current find match.
pub const MATCH_OPACITY: f32 = 0.2;
/// The opacity of a break marker's background.
pub const MARKER_OPACITY: f32 = 0.25;
