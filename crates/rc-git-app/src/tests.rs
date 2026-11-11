use std::fmt::Write as _;
use std::sync::Arc;

use gpui::{AppContext, HeadlessAppContext, TestAppContext, VisualTestContext, px, size};
use rc_git_highlight::{Class, Language, Span};
use rc_git_kit::text_document::TextDocument;

use crate::line_starts;
use crate::viewer::{FileViewer, display_line};

fn document(text: &str) -> Arc<TextDocument> {
    let bytes = text.as_bytes().to_vec();
    let starts = line_starts(&bytes);
    let valid = std::str::from_utf8(&bytes).is_ok();
    Arc::new(TextDocument::new(bytes, &starts, valid))
}

#[test]
fn line_starts_follow_the_core_rule() {
    assert_eq!(line_starts(b""), Vec::<usize>::new());
    assert_eq!(line_starts(b"a"), vec![0]);
    assert_eq!(line_starts(b"a\n"), vec![0]);
    assert_eq!(line_starts(b"a\nb"), vec![0, 2]);
    assert_eq!(line_starts(b"\n\n"), vec![0, 1]);
}

#[test]
fn a_display_line_drops_its_line_break() {
    let document = document("one\r\ntwo\nthree");
    assert_eq!(display_line(&document, 0).text.as_ref(), "one");
    assert_eq!(display_line(&document, 1).text.as_ref(), "two");
    assert_eq!(display_line(&document, 2).text.as_ref(), "three");
    assert_eq!(display_line(&document, 2).range, 9..14);
    assert_eq!(display_line(&document, 3).text.as_ref(), "");
}

#[test]
fn a_break_git_does_not_count_keeps_its_byte_width() {
    let document = document("a\rb\u{2028}c\n");
    let line = display_line(&document, 0);
    assert_eq!(line.text.as_ref(), "a b   c");
    assert_eq!(line.text.len(), line.range.len());
    assert_eq!(line.markers, vec![1..2, 3..6]);
}

#[test]
fn runs_skip_markers_and_stay_sorted() {
    let document = document("ab\rcd\n");
    let line = display_line(&document, 0);
    let spans = [Span {
        start: 0,
        end: 5,
        class: Class::String,
    }];
    let palette = palette();
    let runs = crate::viewer::runs(&line, &spans, palette);
    let ranges: Vec<_> = runs.iter().map(|(range, _)| range.clone()).collect();
    assert_eq!(ranges, vec![0..2, 2..3, 3..5]);
    assert_eq!(runs[1].1.color, None);
    assert_eq!(runs[0].1.color, Some(palette.color(Class::String)));
}

#[test]
fn spans_clip_to_the_line() {
    let document = document("first\nsecond\n");
    let line = display_line(&document, 1);
    let spans = [
        Span {
            start: 0,
            end: 5,
            class: Class::Keyword,
        },
        Span {
            start: 4,
            end: 9,
            class: Class::String,
        },
    ];
    let ranges: Vec<_> = crate::viewer::runs(&line, &spans, palette())
        .into_iter()
        .map(|(range, _)| range)
        .collect();
    assert_eq!(ranges, vec![0..3]);
}

fn palette() -> crate::palette::Palette {
    let cx = TestAppContext::single();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::palette::Palette::current(cx)
    })
}

#[gpui::test]
fn a_100k_line_file_shapes_only_visible_rows(cx: &mut TestAppContext) {
    let mut text = String::new();
    for index in 0..100_000 {
        writeln!(text, "let value_{index} = \"{index}\"; // line").unwrap();
    }
    let document = document(&text);
    cx.update(gpui_component::init);
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), |_, cx| {
            cx.new(|_| FileViewer::new(document))
        })
    });
    let window = window.unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let shaped = window.update(&mut cx, |viewer, _, _| viewer.shaped_rows());
    assert!(
        matches!(shaped, Ok(rows) if rows > 0 && rows < 500),
        "shaped {shaped:?} of 100000"
    );
}

#[gpui::test]
fn spans_arriving_after_first_paint_recolor_without_reshaping_the_file(cx: &mut TestAppContext) {
    let source = "fn main() {}\n".repeat(50_000);
    let spans = rc_git_highlight::highlight(source.as_bytes(), Language::Rust, None);
    let spans = spans.unwrap();
    let document = document(&source);
    cx.update(gpui_component::init);
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), |_, cx| {
            cx.new(|_| FileViewer::new(document))
        })
    });
    let window = window.unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let updated = window.update(&mut cx, |viewer, _, cx| {
        viewer.set_spans(Arc::new(spans), cx);
    });
    assert!(updated.is_ok());
    cx.run_until_parked();
    let shaped = window.update(&mut cx, |viewer, _, _| viewer.shaped_rows());
    assert!(
        matches!(shaped, Ok(rows) if rows < 1_000),
        "shaped {shaped:?} across two paints"
    );
}

#[gpui::test]
fn a_lossy_document_keeps_no_spans(cx: &mut TestAppContext) {
    let bytes = b"fn \xff main() {}\n".to_vec();
    let starts = line_starts(&bytes);
    let document = Arc::new(TextDocument::new(bytes, &starts, false));
    let viewer = cx.new(|_| FileViewer::new(document));
    let spans = Arc::new(vec![Span {
        start: 0,
        end: 2,
        class: Class::Keyword,
    }]);
    viewer.update(cx, |viewer, cx| viewer.set_spans(spans, cx));
    let kept = viewer.read_with(cx, |viewer, _| viewer.span_count());
    assert_eq!(kept, 0);
}

/// Draws through real CoreText and Metal, off screen. A build without glyphs (font-kit off)
/// paints only the background and fails here; the no-op text system above cannot tell.
#[test]
fn the_viewer_puts_ink_on_the_page() {
    let text_system = Arc::new(gpui_apple::AppleTextSystem::new());
    let mut cx = HeadlessAppContext::with_platform(text_system, Arc::new(()), || {
        gpui_platform::current_headless_renderer()
    });
    cx.update(gpui_component::init);
    let source =
        "fn main() {\n    let answer = 42; // comment\n    println!(\"{answer}\");\n}\n".repeat(20);
    let spans = rc_git_highlight::highlight(source.as_bytes(), Language::Rust, None);
    let spans = spans.unwrap();
    let document = document(&source);
    let window = cx.open_window(size(px(800.), px(500.)), |_, cx| {
        cx.new(|cx| {
            let mut viewer = FileViewer::new(document);
            viewer.set_spans(Arc::new(spans), cx);
            viewer
        })
    });
    let window = window.unwrap();
    cx.run_until_parked();
    let image = cx.capture_screenshot(window.into()).unwrap();
    let mut ink = 0usize;
    let mut colored = 0usize;
    for pixel in image.pixels() {
        let [r, g, b, _] = pixel.0;
        if r < 200 || g < 200 || b < 200 {
            ink += 1;
        }
        if r.abs_diff(g) > 60 || g.abs_diff(b) > 60 {
            colored += 1;
        }
    }
    assert!(ink > 2_000, "only {ink} dark pixels: no glyphs drawn");
    assert!(
        colored > 200,
        "only {colored} colored pixels: spans not drawn"
    );
}
