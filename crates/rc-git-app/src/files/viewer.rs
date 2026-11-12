//! The source-mode file viewer (`FileViewController`'s text view, `LineNumberRulerView` and
//! `LinkTextView`): one row per git line, shaped only while on screen, colored from
//! `rc-git-highlight`'s byte spans, with selection, copy, find matches and import links drawn
//! as overlays on the rows they touch.
//!
//! Long lines follow [`WrapLines`], as the diff view's do (see [`crate::list_rows`]); one-line
//! rows scroll sideways as far as [`TextDocument::widest_line`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, ClipboardItem, Context, CursorStyle, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, Hsla, InteractiveElement, IntoElement, ListHorizontalSizingBehavior,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render,
    SharedString, Styled, StyledText, TextLayout, UnderlineStyle, Window, div, list,
    prelude::FluentBuilder, uniform_list,
};
use rc_git_highlight::Span;
use rc_git_kit::import_links::ImportLinkMap;
use rc_git_kit::text_document::TextDocument;
use rc_git_kit::text_find::{Selection, line_of, selected_text};

use crate::list_rows::{RowScroll, WrapLines};
use crate::palette::Palette;
use crate::style;

/// What the viewer asks of the pane that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerEvent {
    /// The pointer moved over the text for the first time: the import links are wanted.
    WantsImportLinks,
    /// An import link was clicked; its specifier, for the model to resolve.
    OpenImport(String),
}

/// A text document on screen. Holds the decoded text, its spans and what is drawn over it;
/// rows are built on demand for the visible range only.
pub struct FileViewer {
    document: Arc<TextDocument>,
    spans: Arc<[Span]>,
    /// Rows handed to the list since creation: the "shape only what is on screen" budget.
    shaped_rows: Rc<Cell<usize>>,
    focus: FocusHandle,
    scroll: RowScroll,
    /// The line the one-line list measures for its sideways scroll.
    widest: Option<usize>,
    selection: Selection,
    dragging: bool,
    /// The laid-out text of each row painted last frame, for hit-testing the pointer.
    layouts: Rc<RefCell<HashMap<usize, TextLayout>>>,
    matches: Arc<[Range<usize>]>,
    current_match: Option<usize>,
    links: Option<ImportLinkMap>,
    links_requested: bool,
    hovered_link: Option<Range<usize>>,
}

impl EventEmitter<ViewerEvent> for FileViewer {}

impl Focusable for FileViewer {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileViewer {
    /// A viewer over `document`, uncolored until [`FileViewer::set_spans`].
    pub fn new(document: Arc<TextDocument>, cx: &mut Context<Self>) -> Self {
        Self {
            widest: document.widest_line(),
            scroll: RowScroll::new(document.line_starts.len(), cx),
            document,
            spans: Arc::new([]),
            shaped_rows: Rc::default(),
            focus: cx.focus_handle(),
            selection: Selection::default(),
            dragging: false,
            layouts: Rc::default(),
            matches: Arc::new([]),
            current_match: None,
            links: None,
            links_requested: false,
            hovered_link: None,
        }
    }

    /// The document on screen.
    #[must_use]
    pub fn document(&self) -> &Arc<TextDocument> {
        &self.document
    }

    /// Colors the document. Spans are UTF-8 byte ranges of the original bytes, so a lossy
    /// document drops them: each U+FFFD shifts every later offset.
    pub fn set_spans(&mut self, spans: Arc<[Span]>, cx: &mut Context<Self>) {
        if self.document.is_lossy {
            return;
        }
        self.spans = spans;
        cx.notify();
    }

    /// Hands the viewer the import links it asked for through
    /// [`ViewerEvent::WantsImportLinks`].
    pub fn set_import_links(&mut self, links: ImportLinkMap, cx: &mut Context<Self>) {
        self.links = Some(links);
        cx.notify();
    }

    /// The find matches to draw, and which one is current.
    pub fn set_matches(
        &mut self,
        matches: Arc<[Range<usize>]>,
        current: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.matches = matches;
        self.current_match = current;
        if let Some(range) = current.and_then(|index| self.matches.get(index)).cloned() {
            self.selection = Selection {
                anchor: range.start,
                head: range.end,
            };
            self.scroll_to_offset(range.start);
        }
        cx.notify();
    }

    /// Selects `range` and scrolls it into view: a revealed search match.
    pub fn reveal(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selection = Selection {
            anchor: range.start,
            head: range.end,
        };
        self.scroll_to_offset(range.start);
        cx.notify();
    }

    /// The selection, as document bytes.
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// The selected text, for ⌘C and "Use Selection for Find".
    #[must_use]
    pub fn selected_text(&self) -> &str {
        selected_text(&self.document.text, &self.selection)
    }

    /// Copies the selection to the pasteboard; nothing when it is empty.
    pub fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }

    /// Selects the whole document.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection = Selection {
            anchor: 0,
            head: self.document.text.len(),
        };
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

    /// Both lists' scroll, for the tests that read where the rows are.
    #[cfg(test)]
    pub(crate) fn row_scroll(&self) -> &RowScroll {
        &self.scroll
    }

    fn scroll_to_offset(&self, offset: usize) {
        self.scroll
            .reveal(line_of(&self.document.line_starts, offset));
    }

    /// The document offset under `position` on row `index`.
    fn offset_at(&self, index: usize, position: Point<Pixels>) -> Option<usize> {
        let line = display_line(&self.document, index);
        let layouts = self.layouts.borrow();
        let layout = layouts.get(&index)?;
        let column = match layout.index_for_position(position) {
            Ok(column) | Err(column) => column,
        };
        Some(line.range.start + column.min(line.range.len()))
    }

    fn press(&mut self, index: usize, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(offset) = self.offset_at(index, event.position) else {
            return;
        };
        if let Some(hit) = self.links.as_ref().and_then(|links| links.hit(offset)) {
            cx.emit(ViewerEvent::OpenImport(hit.specifier));
            return;
        }
        self.selection = if event.modifiers.shift {
            Selection {
                anchor: self.selection.anchor,
                head: offset,
            }
        } else {
            Selection::at(offset)
        };
        self.dragging = true;
        cx.notify();
    }

    fn moved(&mut self, index: usize, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.links_requested {
            self.links_requested = true;
            cx.emit(ViewerEvent::WantsImportLinks);
        }
        let Some(offset) = self.offset_at(index, event.position) else {
            return;
        };
        if self.dragging && event.pressed_button == Some(MouseButton::Left) {
            self.selection.head = offset;
            cx.notify();
            return;
        }
        let hovered = self
            .links
            .as_ref()
            .and_then(|links| links.hit(offset))
            .map(|hit| hit.range);
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            cx.notify();
        }
    }

    fn row(&self, index: usize, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let line = display_line(&self.document, index);
        let mut overlays = Vec::new();
        for (match_index, range) in self.matches.iter().enumerate() {
            if let Some(local) = clip(range, &line.range) {
                let opacity = if Some(match_index) == self.current_match {
                    style::SELECTION_OPACITY
                } else {
                    style::MATCH_OPACITY
                };
                overlays.push((local, background(palette.find(), opacity)));
            }
        }
        if let Some(local) = self.selection.within(&line.range) {
            overlays.push((
                local,
                background(palette.selection(), style::SELECTION_OPACITY),
            ));
        }
        if let Some(local) = self
            .hovered_link
            .as_ref()
            .and_then(|r| clip(r, &line.range))
        {
            overlays.push((local, underline(palette.link())));
        }
        let base = runs(&line, &self.spans, palette);
        let highlights = layer(base, &overlays, line.text.len());
        let text = StyledText::new(line.text).with_highlights(highlights);
        self.layouts
            .borrow_mut()
            .insert(index, text.layout().clone());
        let over_link = self.hovered_link.is_some();
        let wraps = self.scroll.wraps();
        div()
            .id(index)
            .flex()
            .when(wraps, Styled::w_full)
            .when(!wraps, Styled::whitespace_nowrap)
            .font_family(style::MONOSPACE_FONT)
            .text_size(style::TEXT_MONOSPACE)
            .text_color(palette.foreground())
            .when_cursor(over_link)
            .child(
                div()
                    .w(style::GUTTER_WIDTH)
                    .pr(style::GUTTER_GAP)
                    .flex_none()
                    .flex()
                    .justify_end()
                    .text_color(palette.muted())
                    .child(SharedString::from((index + 1).to_string())),
            )
            .child(
                div()
                    .when(wraps, |cell| cell.flex_1().min_w_0())
                    .when(!wraps, Styled::flex_none)
                    .child(text),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| this.press(index, event, cx)),
            )
            .on_mouse_move(cx.listener(move |this, event, _, cx| this.moved(index, event, cx)))
            .into_any_element()
    }
}

/// Sets the pointer over a row: a hand while an import link is under it.
trait WhenCursor {
    fn when_cursor(self, over_link: bool) -> Self;
}

impl<E: Styled> WhenCursor for E {
    fn when_cursor(self, over_link: bool) -> Self {
        if over_link {
            self.cursor(CursorStyle::PointingHand)
        } else {
            self.cursor(CursorStyle::IBeam)
        }
    }
}

impl Render for FileViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::current(cx);
        self.scroll.follow(WrapLines::on(cx));
        self.layouts.borrow_mut().clear();
        let body = if self.scroll.wraps() {
            list(
                self.scroll.wrapped().clone(),
                cx.processor(move |this, index: usize, _window, cx| {
                    this.shaped_rows.set(this.shaped_rows.get() + 1);
                    this.row(index, palette, cx)
                }),
            )
            .size_full()
            .into_any_element()
        } else {
            uniform_list(
                "file-lines",
                self.document.line_starts.len(),
                cx.processor(move |this, range: Range<usize>, _window, cx| {
                    this.shaped_rows.set(this.shaped_rows.get() + range.len());
                    let mut rows = Vec::with_capacity(range.len());
                    for index in range {
                        rows.push(this.row(index, palette, cx));
                    }
                    rows
                }),
            )
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(self.widest)
            .track_scroll(self.scroll.one_line())
            .size_full()
            .into_any_element()
        };
        div()
            .id("file-viewer")
            .key_context("FileViewer")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .on_action(cx.listener(|this, _: &gpui_component::input::Copy, _, cx| this.copy(cx)))
            .on_action(
                cx.listener(|this, _: &gpui_component::input::SelectAll, _, cx| {
                    this.select_all(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = false),
            )
            .child(body)
            .child(self.scroll.scrollbar())
    }
}

/// `range` (document bytes) relative to `line`, when they meet and the part is not empty.
fn clip(range: &Range<usize>, line: &Range<usize>) -> Option<Range<usize>> {
    let start = range.start.max(line.start);
    let end = range.end.min(line.end);
    (start < end).then(|| start - line.start..end - line.start)
}

fn background(color: Hsla, opacity: f32) -> HighlightStyle {
    HighlightStyle {
        background_color: Some(color.opacity(opacity)),
        ..HighlightStyle::default()
    }
}

fn underline(color: Hsla) -> HighlightStyle {
    HighlightStyle {
        underline: Some(UnderlineStyle {
            thickness: style::HAIRLINE,
            color: Some(color),
            wavy: false,
        }),
        ..HighlightStyle::default()
    }
}

/// `base` (sorted, disjoint) with each of `overlays` merged on top, in order, over a line
/// `length` bytes long. The result is sorted and disjoint, as `StyledText` requires; an
/// overlay covering text no base run colors adds a run of its own.
#[must_use]
pub fn layer(
    base: Vec<(Range<usize>, HighlightStyle)>,
    overlays: &[(Range<usize>, HighlightStyle)],
    length: usize,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if overlays.is_empty() {
        return base;
    }
    let mut cuts = vec![0, length];
    for (range, _) in base.iter().chain(overlays) {
        cuts.push(range.start.min(length));
        cuts.push(range.end.min(length));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let [start, end] = [pair[0], pair[1]];
        let mut style: Option<HighlightStyle> = None;
        for (range, run) in base.iter().chain(overlays) {
            if range.start <= start && end <= range.end {
                style = Some(style.map_or(*run, |style| style.highlight(*run)));
            }
        }
        if let Some(style) = style {
            out.push((start..end, style));
        }
    }
    out
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
        colored.push((
            marker.clone(),
            background(palette.muted(), style::MARKER_OPACITY),
        ));
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
