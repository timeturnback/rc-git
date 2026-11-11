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

/// The width of one line-number column of a diff's gutter (old, then new).
pub const DIFF_NUMBER_WIDTH: Pixels = px(40.);
/// The opacity of an added or removed diff row's background tint.
pub const DIFF_TINT_OPACITY: f32 = 0.12;
/// The width of a History lane in the graph column.
pub const GRAPH_LANE_WIDTH: Pixels = px(10.);
/// The space either side of the graph's lanes.
pub const GRAPH_PADDING: Pixels = px(4.);
/// The stroke of a graph lane.
pub const GRAPH_STROKE: Pixels = px(1.5);
/// The radius of a commit's dot in the graph; a merge's is larger.
pub const GRAPH_DOT: Pixels = px(3.);
/// See [`GRAPH_DOT`].
pub const GRAPH_MERGE_DOT: Pixels = px(4.);
/// The History list's short-hash column.
pub const HASH_COLUMN_WIDTH: Pixels = px(72.);
/// The History list's author column.
pub const AUTHOR_COLUMN_WIDTH: Pixels = px(140.);
/// The History list's date column.
pub const DATE_COLUMN_WIDTH: Pixels = px(170.);
/// The History search field's narrowest width.
pub const SEARCH_FIELD_MIN_WIDTH: Pixels = px(160.);
/// The width of a History date filter field.
pub const DAY_FIELD_WIDTH: Pixels = px(110.);
/// How many lines of a commit body the detail header shows before it clips.
pub const DETAIL_BODY_LINES: usize = 6;
/// How many lines of a commit title the detail header shows before it clips.
pub const DETAIL_TITLE_LINES: usize = 3;
/// The blame window's size when it first opens.
pub const BLAME_WINDOW_WIDTH: Pixels = px(640.);
/// See [`BLAME_WINDOW_WIDTH`].
pub const BLAME_WINDOW_HEIGHT: Pixels = px(400.);
/// The blame window's Lines, Commit, Author and Date columns.
pub const BLAME_LINES_WIDTH: Pixels = px(80.);
/// See [`BLAME_LINES_WIDTH`].
pub const BLAME_AUTHOR_WIDTH: Pixels = px(130.);
/// See [`BLAME_LINES_WIDTH`].
pub const BLAME_DATE_WIDTH: Pixels = px(130.);
/// The height of a History commit row and a changed-file row.
pub const LIST_ROW_HEIGHT: Pixels = px(24.);
