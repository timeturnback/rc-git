//! Spacing tokens and the type ramp (`Style.swift`), so the sidebar and the detail pane line up
//! and no view carries a layout number of its own. Colors are not here: every view resolves
//! them from the active theme at render, so the window follows the system appearance.

use gpui::{Pixels, px};

/// The horizontal margin inside the sidebar and in the bars across the top of a pane; a
/// list's selection is inset by the same amount, so a banner above it lines up.
pub const MARGIN: Pixels = px(12.);
/// The space above and below the controls of a bar across the top of a pane.
pub const BAR_PADDING: Pixels = px(8.);
/// The gap between a row's icon and its label, and between neighboring controls.
pub const ROW_GAP: Pixels = px(6.);
/// The gap between groups of toolbar controls.
pub const GROUP_GAP: Pixels = px(16.);
/// How far each folder level of the tree indents.
pub const FOLDER_INDENT: Pixels = px(12.);
/// The size of a row or toolbar icon.
pub const ICON_SIZE: Pixels = px(16.);
/// The height of a segment in the toolbar's mode switch.
pub const SEGMENT_HEIGHT: Pixels = px(24.);
/// The track around and between the mode switch's segments.
pub const SEGMENT_INSET: Pixels = px(2.);
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
/// The corner radius of a row, a banner, a find bar.
pub const RADIUS: Pixels = px(6.);
/// The corner radius of a floating panel: Go to File, Find in Repository, Open Recent, the
/// ref picker.
pub const RADIUS_LG: Pixels = px(10.);

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
pub const TEXT_CAPTION: Pixels = px(12.);
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
/// The grab band straddling a History column edge.
pub const COLUMN_HANDLE_WIDTH: Pixels = px(6.);
/// Added to the widest date's shaped width so rounding never clips it.
pub const COLUMN_FIT_SLACK: Pixels = px(2.);
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
pub const LIST_ROW_HEIGHT: Pixels = px(28.);

/// A kit size in points as GPUI pixels.
#[expect(
    clippy::cast_possible_truncation,
    reason = "layout sizes in points, far inside f32's range"
)]
pub const fn points(value: f64) -> Pixels {
    px(value as f32)
}

/// GPUI pixels as a kit size in points.
pub fn value(pixels: Pixels) -> f64 {
    f64::from(f32::from(pixels))
}

/// The Changes list's floor: it keeps a usable height but yields before the sidebar breaks.
pub const CHANGES_FILES_FLOOR: Pixels = points(rc_git_kit::layout_policy::CHANGES_FILES_FLOOR);
/// The stash list's height in the Changes sidebar.
pub const CHANGES_STASH_HEIGHT: Pixels = points(rc_git_kit::layout_policy::CHANGES_STASH_HEIGHT);
/// The commit message box's height.
pub const CHANGES_MESSAGE_HEIGHT: Pixels =
    points(rc_git_kit::layout_policy::CHANGES_MESSAGE_HEIGHT);
/// The worktree diff pane's floor.
pub const CHANGES_DIFF_FLOOR: Pixels = points(rc_git_kit::layout_policy::CHANGES_DIFF_FLOOR);
/// The height of a Changes row, which carries a tick box beside its label.
pub const CHANGE_ROW_HEIGHT: Pixels = px(26.);
/// The side of a tick box.
pub const TICK_SIZE: Pixels = px(14.);
/// The corner radius of a tick box.
pub const TICK_RADIUS: Pixels = px(4.);
/// A Changes row's status letter, smaller than the name beside it.
pub const TEXT_STATUS_MARK: Pixels = px(10.);
/// The width a dialog's fields take.
pub const SHEET_FIELD_WIDTH: Pixels = px(300.);
/// Markdown preview: the body and code text sizes (`MarkdownRenderer.Theme`).
pub const MARKDOWN_BODY: Pixels = px(13.);
/// See [`MARKDOWN_BODY`].
pub const MARKDOWN_CODE: Pixels = px(12.);
/// The heading sizes, level 1 to 4; levels 5 and 6 take the last.
pub const MARKDOWN_HEADINGS: [Pixels; 5] = [px(22.), px(18.), px(15.), px(13.), px(12.)];
/// How far each list level indents, and the width of the marker column.
pub const MARKDOWN_LIST_INDENT: Pixels = px(22.);
/// See [`MARKDOWN_LIST_INDENT`].
pub const MARKDOWN_MARKER_WIDTH: Pixels = px(20.);
/// The space after a block, and before a level 1 or 2 heading or any other heading.
pub const MARKDOWN_BLOCK_SPACING: Pixels = px(10.);
/// See [`MARKDOWN_BLOCK_SPACING`].
pub const MARKDOWN_MAJOR_HEADING_SPACING: Pixels = px(14.);
/// See [`MARKDOWN_BLOCK_SPACING`].
pub const MARKDOWN_HEADING_SPACING: Pixels = px(8.);
/// The bar down the left of a quote, and the space between it and the text.
pub const MARKDOWN_QUOTE_BAR: Pixels = px(3.);
/// See [`MARKDOWN_QUOTE_BAR`].
pub const MARKDOWN_QUOTE_PADDING: Pixels = px(10.);
/// The padding inside a code panel and a table cell.
pub const MARKDOWN_PANEL_PADDING: Pixels = px(8.);
/// See [`MARKDOWN_PANEL_PADDING`].
pub const MARKDOWN_CELL_PADDING: Pixels = px(6.);
/// The margin around the whole preview.
pub const MARKDOWN_MARGIN: Pixels = px(16.);
/// The opacity of a code panel or a table header over the background.
pub const MARKDOWN_CODE_OPACITY: f32 = 0.05;

/// The Go to File panel's size (`GoToFileController`).
pub const GO_TO_FILE_WIDTH: Pixels = px(560.);
/// See [`GO_TO_FILE_WIDTH`].
pub const GO_TO_FILE_HEIGHT: Pixels = px(380.);
/// The Find in Repository panel's size (`FindInRepositoryController`).
pub const FIND_PANEL_WIDTH: Pixels = px(720.);
/// See [`FIND_PANEL_WIDTH`].
pub const FIND_PANEL_HEIGHT: Pixels = px(460.);
/// The Open Recent Repository panel's size (`RecentRepositoryPicker`).
pub const RECENT_PANEL_WIDTH: Pixels = px(560.);
/// See [`RECENT_PANEL_WIDTH`].
pub const RECENT_PANEL_HEIGHT: Pixels = px(320.);
/// How far below the toolbar a quick panel hangs.
pub const PANEL_TOP: Pixels = px(48.);
/// A quick panel's field text, larger than the rows under it.
pub const TEXT_PANEL_FIELD: Pixels = px(16.);
/// The Settings window's size when it first opens (`SettingsWindowController`).
pub const SETTINGS_WIDTH: Pixels = px(560.);
/// See [`SETTINGS_WIDTH`].
pub const SETTINGS_HEIGHT: Pixels = px(320.);
/// The Settings host column's width.
pub const SETTINGS_HOST_WIDTH: Pixels = px(180.);
/// The update window's size when it first opens (`UpdateWindowController`).
pub const UPDATE_WIDTH: Pixels = px(620.);
/// See [`UPDATE_WIDTH`].
pub const UPDATE_HEIGHT: Pixels = px(440.);
/// The smallest the update window gets.
pub const UPDATE_MIN_WIDTH: Pixels = px(460.);
/// See [`UPDATE_MIN_WIDTH`].
pub const UPDATE_MIN_HEIGHT: Pixels = px(320.);
/// The update log's floor, so the header and footer never squeeze it shut.
pub const UPDATE_LOG_MIN_HEIGHT: Pixels = px(120.);
/// The About window's size.
pub const ABOUT_WIDTH: Pixels = px(340.);
/// See [`ABOUT_WIDTH`].
pub const ABOUT_HEIGHT: Pixels = px(200.);
/// The Changelog window's opening size.
pub const CHANGELOG_WIDTH: Pixels = px(620.);
/// See [`CHANGELOG_WIDTH`].
pub const CHANGELOG_HEIGHT: Pixels = px(680.);
/// The smallest the Changelog window gets.
pub const CHANGELOG_MIN_WIDTH: Pixels = px(360.);
/// See [`CHANGELOG_MIN_WIDTH`].
pub const CHANGELOG_MIN_HEIGHT: Pixels = px(240.);
