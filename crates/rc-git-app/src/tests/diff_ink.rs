//! The diff view's long lines, laid out through real CoreText off screen: one-line rows scroll
//! sideways as far as the widest row, wrapped rows grow taller instead, and switching between
//! the two keeps the top row in view.

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, HeadlessAppContext, IntoElement, ParentElement, Render, Styled,
    Window, WindowHandle, div, px, size,
};
use rc_git_core::{DiffHunk, DiffLine, DiffLineKind, LineEnd};

use crate::diff::{DiffContent, DiffView};
use crate::list_rows::WrapLines;
use crate::tests::support::headless_app;

const ROWS: usize = 5_000;
const LONG_ROW: usize = 3;

/// `ROWS - 1` short lines with one far wider than any pane at row [`LONG_ROW`].
fn hunks() -> Arc<[DiffHunk]> {
    let mut lines = Vec::with_capacity(ROWS - 1);
    for index in 0..ROWS - 1 {
        let text = if index + 1 == LONG_ROW {
            "wide ".repeat(800)
        } else {
            format!("line {index}")
        };
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: text.into(),
            eol: LineEnd::Lf,
        });
    }
    let count = u32::try_from(lines.len()).unwrap();
    vec![DiffHunk {
        old_start: 0,
        old_lines: 0,
        new_start: 1,
        new_lines: count,
        lines,
    }]
    .into()
}

/// The window's width, and the column [`Split`] keeps left of the diff.
const WINDOW_WIDTH: f32 = 800.;
const COLUMN_WIDTH: f32 = 200.;

/// The diff laid out the way History lays it out: a `resizable_panel` is a flex row beside a
/// fixed column, and the diff and its path bar are a column inside it, so the diff is a flex
/// item whose minimum width is whatever its content says it is.
struct Split {
    diff: Entity<DiffView>,
}

impl Render for Split {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(div().w(px(COLUMN_WIDTH)).flex_none())
            .child(
                div().flex().flex_grow_1().size_full().child(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .child(div().h(px(24.)))
                        .child(div().flex_1().min_h_0().child(self.diff.clone())),
                ),
            )
    }
}

fn open(cx: &mut HeadlessAppContext, wraps: bool) -> (WindowHandle<Split>, Entity<DiffView>) {
    cx.update(|cx| cx.set_global(WrapLines(wraps)));
    let handle = cx
        .open_window(size(px(WINDOW_WIDTH), px(600.)), |_, cx| {
            let diff = cx.new(DiffView::new);
            diff.update(cx, |view, cx| {
                view.set_content(DiffContent::Hunks(hunks()), cx);
            });
            cx.new(|_| Split { diff })
        })
        .unwrap();
    let view = cx.update(|cx| handle.update(cx, |split, _, _| split.diff.clone()).unwrap());
    draw(cx, handle);
    (handle, view)
}

fn draw(cx: &mut HeadlessAppContext, handle: WindowHandle<Split>) {
    cx.run_until_parked();
    cx.capture_screenshot(handle.into()).unwrap();
}

#[test]
fn a_long_line_scrolls_sideways_when_lines_do_not_wrap() {
    let mut cx = headless_app();
    let (_, view) = open(&mut cx, false);
    let (content, viewport) = cx
        .update(|cx| view.read(cx).row_scroll().scroll_widths())
        .unwrap();
    assert!(
        viewport <= px(WINDOW_WIDTH - COLUMN_WIDTH),
        "the viewport {viewport:?} is the pane's width, not the long line's"
    );
    assert!(
        content > viewport * 4.,
        "content {content:?} is the long line's width, past the viewport {viewport:?}"
    );
}

#[test]
fn wrapped_rows_grow_taller_and_only_the_rows_on_screen_are_built() {
    let mut cx = headless_app();
    let (_, view) = open(&mut cx, true);
    let (short, long, built) = cx.update(|cx| {
        let view = view.read(cx);
        (
            view.row_scroll().wrapped_row_height(1).unwrap(),
            view.row_scroll().wrapped_row_height(LONG_ROW).unwrap(),
            view.built_rows(),
        )
    });
    assert!(
        long > short * 4.,
        "the long row wraps: {long:?} over {short:?}"
    );
    assert!(built > 0 && built < ROWS, "{built} rows built of {ROWS}");
}

#[test]
fn switching_wrap_keeps_the_top_row() {
    let mut cx = headless_app();
    let (handle, view) = open(&mut cx, false);
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.row_scroll().scroll_to_row(2_000);
            cx.notify();
        });
    });
    draw(&mut cx, handle);
    assert_eq!(cx.update(|cx| view.read(cx).row_scroll().top_row()), 2_000);

    cx.update(|cx| {
        cx.set_global(WrapLines(true));
        cx.refresh_windows();
    });
    draw(&mut cx, handle);
    assert!(cx.update(|cx| view.read(cx).row_scroll().wraps()));
    assert_eq!(cx.update(|cx| view.read(cx).row_scroll().top_row()), 2_000);

    cx.update(|cx| {
        cx.set_global(WrapLines(false));
        cx.refresh_windows();
    });
    draw(&mut cx, handle);
    assert!(!cx.update(|cx| view.read(cx).row_scroll().wraps()));
    assert_eq!(cx.update(|cx| view.read(cx).row_scroll().top_row()), 2_000);
}

#[test]
fn a_sideways_scroll_moves_the_one_line_rows() {
    let mut cx = headless_app();
    let (handle, view) = open(&mut cx, false);
    let event = gpui::ScrollEvent {
        position: gpui::point(px(COLUMN_WIDTH + 100.), px(100.)),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(-120.), px(0.))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    };
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_event(gpui::PlatformInput::Scroll(event), cx);
    })
    .unwrap();
    draw(&mut cx, handle);
    let offset = cx.update(|cx| view.read(cx).row_scroll().scroll_offset());
    assert!(offset.x < px(0.), "scrolled right: {offset:?}");
}
