//! One cell of the History lane graph (`GraphCellView.swift`), painted on a `canvas` from a
//! kit [`GraphRow`]: the lane segments crossing the row, then the commit's dot. Lanes past
//! [`MAX_DRAWN_LANES`] share the last column (`GraphRow::drawn_edges`), so a pathological
//! repository widens nothing. A lane's color is its append-only color index modulo the theme
//! palette, so a lane keeps its color as more pages load.

use gpui::{
    App, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled, Window, canvas, fill,
    point, px,
};
use gpui_component::ActiveTheme;
use rc_git_kit::commit_graph::{GraphRow, MAX_DRAWN_LANES, Phase};

use crate::style;

/// The theme colors lanes cycle through, in the Swift palette's order: blue, green, orange,
/// purple, teal, pink, brown, indigo, each as the nearest theme color.
fn palette(cx: &App) -> [Hsla; 8] {
    let theme = cx.theme();
    [
        theme.blue,
        theme.green,
        theme.yellow,
        theme.magenta,
        theme.cyan,
        theme.red,
        theme.yellow_light,
        theme.blue_light,
    ]
}

/// The color of lane color index `index`.
#[must_use]
pub fn lane_color(index: usize, cx: &App) -> Hsla {
    let colors = palette(cx);
    colors[index % colors.len()]
}

/// The graph column's width for `lanes` lanes, clamped to the drawn cap.
#[must_use]
pub fn width(lanes: usize) -> Pixels {
    let drawn = lanes.clamp(1, MAX_DRAWN_LANES);
    #[expect(
        clippy::cast_precision_loss,
        reason = "at most MAX_DRAWN_LANES lanes, far inside f32's exact range"
    )]
    let drawn = drawn as f32;
    style::GRAPH_LANE_WIDTH * drawn + style::GRAPH_PADDING * 2.
}

fn lane_x(origin: Pixels, lane: usize) -> Pixels {
    let lane = lane.min(MAX_DRAWN_LANES - 1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a lane below MAX_DRAWN_LANES, far inside f32's exact range"
    )]
    let lane = lane as f32;
    origin + style::GRAPH_PADDING + style::GRAPH_LANE_WIDTH * lane + style::GRAPH_LANE_WIDTH / 2.
}

/// One stroke, ready to paint.
struct Stroke {
    from: Point<Pixels>,
    to: Point<Pixels>,
    controls: Option<(Point<Pixels>, Point<Pixels>)>,
    color: Hsla,
}

fn strokes(row: &GraphRow, bounds: Bounds<Pixels>, cx: &App) -> Vec<Stroke> {
    let top = bounds.origin.y;
    let bottom = bounds.origin.y + bounds.size.height;
    let mid = top + bounds.size.height / 2.;
    let quarter = bounds.size.height / 4.;
    let left = bounds.origin.x;
    let mut out = Vec::new();
    for edge in row.drawn_edges(MAX_DRAWN_LANES) {
        let from_x = lane_x(left, edge.from);
        let to_x = lane_x(left, edge.to);
        let color = lane_color(edge.color_index, cx);
        let (from, to, controls) = match edge.phase {
            Phase::Through if edge.from == edge.to => {
                (point(from_x, top), point(to_x, bottom), None)
            }
            Phase::Through => (
                point(from_x, top),
                point(to_x, bottom),
                Some((point(from_x, mid), point(to_x, mid))),
            ),
            Phase::Into => (
                point(from_x, top),
                point(to_x, mid),
                Some((point(from_x, mid), point(to_x, top + quarter))),
            ),
            Phase::Out => (
                point(from_x, mid),
                point(to_x, bottom),
                Some((point(from_x, mid + quarter), point(to_x, mid))),
            ),
        };
        out.push(Stroke {
            from,
            to,
            controls,
            color,
        });
    }
    out
}

fn paint(row: &GraphRow, bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
    for stroke in strokes(row, bounds, cx) {
        let mut path = PathBuilder::stroke(style::GRAPH_STROKE);
        path.move_to(stroke.from);
        match stroke.controls {
            Some((a, b)) => path.cubic_bezier_to(stroke.to, a, b),
            None => path.line_to(stroke.to),
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, stroke.color);
        }
    }
    let radius = if row.is_merge {
        style::GRAPH_MERGE_DOT
    } else {
        style::GRAPH_DOT
    };
    let center = point(
        lane_x(bounds.origin.x, row.commit_lane),
        bounds.origin.y + bounds.size.height / 2.,
    );
    let dot = Bounds::new(
        point(center.x - radius, center.y - radius),
        gpui::size(radius * 2., radius * 2.),
    );
    window.paint_quad(fill(dot, lane_color(row.color_index, cx)).corner_radii(radius));
}

/// The graph cell for `row`, `width` wide and the row's full height.
pub fn cell(row: GraphRow, width: Pixels) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| paint(&row, bounds, window, cx),
    )
    .w(width)
    .h_full()
    .flex_none()
}

/// A graph cell with nothing in it, keeping the column's width.
pub fn empty(width: Pixels) -> impl IntoElement {
    gpui::div().w(width).h_full().flex_none().min_w(px(0.))
}
