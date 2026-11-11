//! The source viewer: line splitting, runs and overlays, the shaping budget, selection, copy,
//! find, and real ink through CoreText and Metal.

use std::fmt::Write as _;
use std::sync::Arc;

use gpui::{
    AppContext, HeadlessAppContext, HighlightStyle, TestAppContext, VisualTestContext, hsla, px,
    size,
};
use rc_git_highlight::{Class, Language, Span};
use rc_git_kit::text_document::TextDocument;
use rc_git_kit::text_find::Selection;

use crate::files::viewer::{FileViewer, display_line, layer, runs};
use crate::tests::support::{document, line_starts, palette};

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
    let runs = runs(&line, &spans, palette);
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
    let ranges: Vec<_> = runs(&line, &spans, palette())
        .into_iter()
        .map(|(range, _)| range)
        .collect();
    assert_eq!(ranges, vec![0..3]);
}

fn colored(red: f32) -> HighlightStyle {
    HighlightStyle {
        color: Some(hsla(red, 1., 0.5, 1.)),
        ..HighlightStyle::default()
    }
}

fn shaded() -> HighlightStyle {
    HighlightStyle {
        background_color: Some(hsla(0.5, 1., 0.5, 0.3)),
        ..HighlightStyle::default()
    }
}

#[test]
fn an_overlay_splits_the_runs_it_crosses_and_keeps_their_color() {
    let base = vec![(0..4, colored(0.1)), (6..10, colored(0.2))];
    let layered = layer(base, &[(2..8, shaded())], 12);
    let ranges: Vec<_> = layered.iter().map(|(range, _)| range.clone()).collect();
    assert_eq!(ranges, vec![0..2, 2..4, 4..6, 6..8, 8..10]);
    assert_eq!(layered[1].1.color, colored(0.1).color);
    assert!(layered[1].1.background_color.is_some());
    assert_eq!(layered[2].1.color, None, "uncolored text under the overlay");
    assert!(layered[2].1.background_color.is_some());
    assert!(layered[4].1.background_color.is_none());
}

#[test]
fn no_overlay_leaves_the_runs_alone() {
    let base = vec![(0..4, colored(0.1))];
    assert_eq!(layer(base.clone(), &[], 4), base);
}

#[test]
fn an_overlay_past_the_line_end_is_clipped() {
    let layered = layer(Vec::new(), &[(2..40, shaded())], 5);
    let ranges: Vec<_> = layered.iter().map(|(range, _)| range.clone()).collect();
    assert_eq!(ranges, vec![2..5]);
}

fn open(cx: &mut TestAppContext, document: Arc<TextDocument>) -> gpui::WindowHandle<FileViewer> {
    cx.update(gpui_component::init);
    let window = cx.update(|cx| {
        cx.open_window(gpui::WindowOptions::default(), |_, cx| {
            cx.new(|cx| FileViewer::new(document, cx))
        })
    });
    window.unwrap_or_else(|error| panic!("open window: {error:#}"))
}

#[gpui::test]
fn a_100k_line_file_shapes_only_visible_rows(cx: &mut TestAppContext) {
    let mut text = String::new();
    for index in 0..100_000 {
        writeln!(text, "let value_{index} = \"{index}\"; // line").unwrap_or(());
    }
    let window = open(cx, document(&text));
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
    let spans = rc_git_highlight::highlight(source.as_bytes(), Language::Rust, None)
        .unwrap_or_else(|error| panic!("{error:?}"));
    let window = open(cx, document(&source));
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let updated = window.update(&mut cx, |viewer, _, cx| {
        viewer.set_spans(spans.into(), cx);
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
    let viewer = cx.new(|cx| FileViewer::new(document, cx));
    let spans: Arc<[Span]> = Arc::new([Span {
        start: 0,
        end: 2,
        class: Class::Keyword,
    }]);
    viewer.update(cx, |viewer, cx| viewer.set_spans(spans, cx));
    let kept = viewer.read_with(cx, |viewer, _| viewer.span_count());
    assert_eq!(kept, 0);
}

#[gpui::test]
fn copy_puts_the_selection_on_the_pasteboard(cx: &mut TestAppContext) {
    let viewer = cx.new(|cx| FileViewer::new(document("alpha\nbeta\n"), cx));
    viewer.update(cx, |viewer, cx| {
        viewer.reveal(2..8, cx);
        viewer.copy(cx);
    });
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("pha\nbe"));
    viewer.update(cx, FileViewer::select_all);
    let all = viewer.read_with(cx, |viewer, _| viewer.selection());
    assert_eq!(
        all,
        Selection {
            anchor: 0,
            head: 11
        }
    );
}

#[gpui::test]
fn an_empty_selection_copies_nothing(cx: &mut TestAppContext) {
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("before".to_owned()));
    let viewer = cx.new(|cx| FileViewer::new(document("alpha\n"), cx));
    viewer.update(cx, FileViewer::copy);
    let kept = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(kept.as_deref(), Some("before"));
}

#[gpui::test]
fn the_current_match_becomes_the_selection(cx: &mut TestAppContext) {
    let viewer = cx.new(|cx| FileViewer::new(document("one two one\n"), cx));
    viewer.update(cx, |viewer, cx| {
        viewer.set_matches(Arc::new([0..3, 8..11]), Some(1), cx);
    });
    let selection = viewer.read_with(cx, |viewer, _| viewer.selection());
    assert_eq!(selection.range(), 8..11);
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
    let spans = rc_git_highlight::highlight(source.as_bytes(), Language::Rust, None)
        .unwrap_or_else(|error| panic!("{error:?}"));
    let document = document(&source);
    let window = cx
        .open_window(size(px(800.), px(500.)), |_, cx| {
            cx.new(|cx| {
                let mut viewer = FileViewer::new(document, cx);
                viewer.set_spans(spans.into(), cx);
                viewer
            })
        })
        .unwrap_or_else(|error| panic!("{error:#}"));
    cx.run_until_parked();
    let image = cx
        .capture_screenshot(window.into())
        .unwrap_or_else(|error| panic!("{error:#}"));
    let (ink, colored) = crate::tests::support::count_ink(image.pixels().map(|pixel| pixel.0));
    assert!(ink > 2_000, "only {ink} dark pixels: no glyphs drawn");
    assert!(
        colored > 200,
        "only {colored} colored pixels: spans not drawn"
    );
}
