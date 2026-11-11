//! The source-mode file viewer: one row per git line, shaped only while on screen, colored from
//! `rc-git-highlight`'s byte spans.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Context, HighlightStyle, Hsla, IntoElement, ParentElement, Render, SharedString, Styled,
    StyledText, Window, div, px, uniform_list,
};
use rc_git_highlight::Span;
use rc_git_kit::text_document::TextDocument;

use crate::palette::Palette;

/// Width of the line-number gutter.
const GUTTER_WIDTH: f32 = 56.;
/// Monospaced text size, matching the `AppKit` viewer.
const TEXT_SIZE: f32 = 12.;

/// A text document on screen. Holds the decoded text and its spans; rows are built on demand
/// for the visible range only.
pub struct FileViewer {
    document: Arc<TextDocument>,
    spans: Arc<Vec<Span>>,
    /// Rows handed to the list since creation: the "shape only what is on screen" budget.
    shaped_rows: Rc<Cell<usize>>,
}

impl FileViewer {
    /// A viewer over `document`, uncolored until [`FileViewer::set_spans`].
    #[must_use]
    pub fn new(document: Arc<TextDocument>) -> Self {
        Self {
            document,
            spans: Arc::default(),
            shaped_rows: Rc::default(),
        }
    }

    /// Colors the document. Spans are UTF-8 byte ranges of the original bytes, so a lossy
    /// document drops them: each U+FFFD shifts every later offset.
    pub fn set_spans(&mut self, spans: Arc<Vec<Span>>, cx: &mut Context<Self>) {
        if self.document.is_lossy {
            return;
        }
        self.spans = spans;
        cx.notify();
    }

    /// How many rows have been built since the viewer was created.
    #[cfg(test)]
    pub(crate) fn shaped_rows(&self) -> usize {
        self.shaped_rows.get()
    }

    #[cfg(test)]
    pub(crate) fn span_count(&self) -> usize {
        self.spans.len()
    }

    fn row(&self, index: usize, palette: Palette) -> impl IntoElement + use<> {
        let line = display_line(&self.document, index);
        let runs = runs(&line, &self.spans, palette);
        div()
            .flex()
            .font_family("Menlo")
            .text_size(px(TEXT_SIZE))
            .text_color(palette.foreground())
            .child(
                div()
                    .w(px(GUTTER_WIDTH))
                    .flex_none()
                    .text_color(palette.muted())
                    .child(format!("{}", index + 1)),
            )
            .child(StyledText::new(line.text).with_highlights(runs))
    }
}

impl Render for FileViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::current(cx);
        div().size_full().child(
            uniform_list(
                "file-lines",
                self.document.line_starts.len(),
                cx.processor(move |this, range: Range<usize>, _window, _cx| {
                    this.shaped_rows.set(this.shaped_rows.get() + range.len());
                    let mut rows = Vec::with_capacity(range.len());
                    for index in range {
                        rows.push(this.row(index, palette));
                    }
                    rows
                }),
            )
            .size_full(),
        )
    }
}

/// One git line ready to shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    /// The bytes of `text` this line covers, without its line break.
    pub range: Range<usize>,
    /// The line's text, with each break git does not count (lone CR, VT, FF, NEL, U+2028,
    /// U+2029) replaced by spaces of the same byte length, so span offsets still line up.
    pub text: SharedString,
    /// Where those breaks were, relative to the line, drawn as markers.
    pub markers: Vec<Range<usize>>,
}

/// Line `index` of `document`; an empty line past the end.
#[must_use]
pub fn display_line(document: &TextDocument, index: usize) -> DisplayLine {
    let text = document.text.as_str();
    let Some(&start) = document.line_starts.get(index) else {
        return DisplayLine {
            range: 0..0,
            text: SharedString::default(),
            markers: Vec::new(),
        };
    };
    let mut end = document
        .line_starts
        .get(index + 1)
        .copied()
        .unwrap_or(text.len());
    let bytes = text.as_bytes();
    if end > start && bytes.get(end - 1) == Some(&b'\n') {
        end -= 1;
    }
    if end > start && bytes.get(end - 1) == Some(&b'\r') {
        end -= 1;
    }
    let first = document
        .non_lf_breaks
        .partition_point(|&offset| offset < start);
    let mut markers = Vec::new();
    let mut line = String::with_capacity(end - start);
    let mut cursor = start;
    for &offset in document.non_lf_breaks.get(first..).unwrap_or_default() {
        if offset >= end {
            break;
        }
        let width = text
            .get(offset..)
            .and_then(|rest| rest.chars().next())
            .map_or(1, char::len_utf8);
        line.push_str(text.get(cursor..offset).unwrap_or_default());
        line.push_str(&" ".repeat(width));
        markers.push(offset - start..offset - start + width);
        cursor = offset + width;
    }
    line.push_str(text.get(cursor..end).unwrap_or_default());
    DisplayLine {
        range: start..end,
        text: line.into(),
        markers,
    }
}

/// Highlight runs for `line`: its spans clipped to the line, with the break markers cut out
/// and drawn on a muted background instead. Sorted and disjoint, as `StyledText` requires.
#[must_use]
pub fn runs(
    line: &DisplayLine,
    spans: &[Span],
    palette: Palette,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut colored = Vec::new();
    let first = spans.partition_point(|span| span.end as usize <= line.range.start);
    for span in spans.get(first..).unwrap_or_default() {
        let start = span.start as usize;
        if start >= line.range.end {
            break;
        }
        let clipped = start.max(line.range.start) - line.range.start
            ..(span.end as usize).min(line.range.end) - line.range.start;
        push_outside_markers(
            &mut colored,
            clipped,
            palette.color(span.class),
            &line.markers,
        );
    }
    for marker in &line.markers {
        let style = HighlightStyle {
            background_color: Some(palette.muted().opacity(0.25)),
            ..HighlightStyle::default()
        };
        colored.push((marker.clone(), style));
    }
    colored.sort_by_key(|(range, _)| range.start);
    colored
}

fn push_outside_markers(
    out: &mut Vec<(Range<usize>, HighlightStyle)>,
    range: Range<usize>,
    color: Hsla,
    markers: &[Range<usize>],
) {
    let style = HighlightStyle {
        color: Some(color),
        ..HighlightStyle::default()
    };
    let mut start = range.start;
    for marker in markers {
        if marker.end <= start || marker.start >= range.end {
            continue;
        }
        if marker.start > start {
            out.push((start..marker.start, style));
        }
        start = marker.end;
    }
    if start < range.end {
        out.push((start..range.end, style));
    }
}
