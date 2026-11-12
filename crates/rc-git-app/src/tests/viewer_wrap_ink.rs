//! The file viewer's long lines, laid out through real CoreText off screen: one-line rows
//! scroll sideways as far as the widest line, wrapped rows grow taller instead, and switching
//! between the two keeps the top row in view.

use std::fmt::Write as _;

use gpui::{AppContext, Entity, HeadlessAppContext, WindowHandle, px, size};

use crate::files::viewer::FileViewer;
use crate::list_rows::WrapLines;
use crate::tests::support::{document, headless_app};

const ROWS: usize = 5_000;
const LONG_ROW: usize = 3;
const WINDOW_WIDTH: f32 = 600.;

/// `ROWS` short lines with one far wider than the window at row [`LONG_ROW`].
fn text() -> String {
    let mut text = String::new();
    for index in 0..ROWS {
        if index == LONG_ROW {
            text.push_str(&"wide ".repeat(800));
            text.push('\n');
        } else {
            writeln!(text, "line {index}").unwrap();
        }
    }
    text
}

fn open(
    cx: &mut HeadlessAppContext,
    wraps: bool,
) -> (WindowHandle<FileViewer>, Entity<FileViewer>) {
    cx.update(|cx| cx.set_global(WrapLines(wraps)));
    let document = document(&text());
    let handle = cx
        .open_window(size(px(WINDOW_WIDTH), px(600.)), |_, cx| {
            cx.new(|cx| FileViewer::new(document, cx))
        })
        .unwrap();
    let view = cx.update(|cx| handle.entity(cx).unwrap());
    draw(cx, handle);
    (handle, view)
}

fn draw(cx: &mut HeadlessAppContext, handle: WindowHandle<FileViewer>) {
    cx.run_until_parked();
    cx.capture_screenshot(handle.into()).unwrap();
}

fn set_wrap(cx: &mut HeadlessAppContext, handle: WindowHandle<FileViewer>, wraps: bool) {
    cx.update(|cx| {
        cx.set_global(WrapLines(wraps));
        cx.refresh_windows();
    });
    draw(cx, handle);
}

#[test]
fn a_long_line_scrolls_sideways_when_lines_do_not_wrap() {
    let mut cx = headless_app();
    let (_, view) = open(&mut cx, false);
    assert!(!cx.update(|cx| view.read(cx).row_scroll().wraps()));
    let (content, viewport) = cx
        .update(|cx| view.read(cx).row_scroll().scroll_widths())
        .unwrap();
    assert!(
        viewport <= px(WINDOW_WIDTH),
        "the viewport {viewport:?} is the window's width, not the long line's"
    );
    assert!(
        content > viewport * 4.,
        "content {content:?} is the long line's width, past the viewport {viewport:?}"
    );
}

#[test]
fn wrapped_rows_grow_taller_and_only_the_rows_on_screen_are_shaped() {
    let mut cx = headless_app();
    let (_, view) = open(&mut cx, true);
    let (short, long, shaped) = cx.update(|cx| {
        let view = view.read(cx);
        (
            view.row_scroll().wrapped_row_height(1).unwrap(),
            view.row_scroll().wrapped_row_height(LONG_ROW).unwrap(),
            view.shaped_rows(),
        )
    });
    assert!(
        long > short * 4.,
        "the long row wraps: {long:?} over {short:?}"
    );
    assert!(
        shaped > 0 && shaped < ROWS,
        "{shaped} rows shaped of {ROWS}"
    );
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

    set_wrap(&mut cx, handle, true);
    assert!(cx.update(|cx| view.read(cx).row_scroll().wraps()));
    assert_eq!(cx.update(|cx| view.read(cx).row_scroll().top_row()), 2_000);

    set_wrap(&mut cx, handle, false);
    assert!(!cx.update(|cx| view.read(cx).row_scroll().wraps()));
    assert_eq!(cx.update(|cx| view.read(cx).row_scroll().top_row()), 2_000);
}
