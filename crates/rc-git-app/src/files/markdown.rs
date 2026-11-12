//! The file pane's markdown preview (`MarkdownRenderer` and the preview half of
//! `FileViewController`): one element per block of kit's [`MarkdownDocument`], laid out in a
//! variable-height list that builds only the blocks on screen. Fenced code is colored by the
//! same highlighter source mode uses; tables are real grid rows.
//!
//! Every link resolves through [`MarkdownLinkTarget::resolve`] against the repository path of
//! the file on screen and nowhere else, so a click, the context menu and Copy Link agree on what
//! a destination means. A repository path goes up to the pane as [`PreviewEvent::OpenPath`]; a
//! `#fragment` scrolls to the heading [`MarkdownDocument::heading_anchors`] names.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    Font, FontStyle, FontWeight, Hsla, InteractiveElement, InteractiveText, IntoElement,
    ListAlignment, ListOffset, ListState, ParentElement, Pixels, PromptLevel, Render, SharedString,
    StrikethroughStyle, Styled, StyledText, Task, TextRun, UnderlineStyle, WeakEntity, Window, div,
    font, list, px,
};
use gpui_component::ActiveTheme;
use gpui_component::menu::{ContextMenuExt, PopupMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use rc_git_highlight::Span;
use rc_git_kit::highlighting::highlight_fence;
use rc_git_kit::markdown::{
    Alignment, BlockKind, MarkdownBlock, MarkdownDocument, MarkdownLinkTarget, MarkdownListMarker,
    MarkdownRun, MarkdownStyle, MarkdownTable,
};
use rc_git_kit::text_document::TextDocument;

use crate::palette::Palette;
use crate::style;

/// Code blocks above this many bytes are shown plain; a preview is not an editor.
pub const HIGHLIGHT_LIMIT: usize = 128 * 1024;

/// A parsed document ready to draw: its blocks, the spans of each fenced block that has a
/// known language, and where each heading and footnote anchor starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// The blocks, in document order.
    pub blocks: Vec<MarkdownBlock>,
    /// Per block, the syntax spans of a fenced code block; `None` for plain.
    pub fences: Vec<Option<Arc<[Span]>>>,
    /// Anchor to the index of the block it names.
    pub anchors: HashMap<String, usize>,
}

impl Rendered {
    /// Parses `source` and colors its fences. A document with no block shows its source as
    /// one plain code block, since an empty pane would hide a file that has bytes in it.
    #[must_use]
    pub fn build(source: &str) -> Self {
        let mut blocks = MarkdownDocument::parse(source).blocks;
        if blocks.is_empty() && !source.is_empty() {
            blocks.push(MarkdownBlock::new(BlockKind::Code {
                language: None,
                text: source.to_owned(),
            }));
        }
        let anchors = anchors(&blocks);
        let fences = blocks.iter().map(fence_spans).collect();
        Self {
            blocks,
            fences,
            anchors,
        }
    }

    /// The block a `#fragment` named `name` scrolls to, by the slug rule headings use.
    #[must_use]
    pub fn anchor_block(&self, name: &str) -> Option<usize> {
        self.anchors.get(&MarkdownDocument::anchor(name)).copied()
    }
}

/// Every heading's de-duplicated anchor and every footnote's, to its block. The first
/// definition of a repeated footnote keeps the anchor; a heading with no slug claims nothing.
fn anchors(blocks: &[MarkdownBlock]) -> HashMap<String, usize> {
    let mut headings = MarkdownDocument::heading_anchors(blocks).into_iter();
    let mut out = HashMap::new();
    for (index, block) in blocks.iter().enumerate() {
        let anchor = match &block.kind {
            BlockKind::Heading { .. } => headings.next().unwrap_or_default(),
            BlockKind::Footnote { label } => MarkdownDocument::footnote_anchor(label),
            BlockKind::Paragraph
            | BlockKind::Code { .. }
            | BlockKind::Table(_)
            | BlockKind::ThematicBreak
            | BlockKind::FrontMatter { .. }
            | BlockKind::Html { .. } => continue,
        };
        if !anchor.is_empty() {
            out.entry(anchor).or_insert(index);
        }
    }
    out
}

/// The spans of a fenced block whose fence names a language, under [`HIGHLIGHT_LIMIT`].
/// Front matter reads as YAML; raw HTML stays plain, as Swift shows it.
fn fence_spans(block: &MarkdownBlock) -> Option<Arc<[Span]>> {
    let (language, text) = match &block.kind {
        BlockKind::Code {
            language: Some(language),
            text,
        } => (language.as_str(), text),
        BlockKind::FrontMatter { text } => ("yaml", text),
        BlockKind::Code { language: None, .. }
        | BlockKind::Paragraph
        | BlockKind::Heading { .. }
        | BlockKind::Table(_)
        | BlockKind::ThematicBreak
        | BlockKind::Html { .. }
        | BlockKind::Footnote { .. } => return None,
    };
    if text.is_empty() || text.len() > HIGHLIGHT_LIMIT {
        return None;
    }
    highlight_fence(language, text.as_bytes())
        .ok()
        .map(Arc::from)
}

/// A link inside a run of inline text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineLink {
    /// The bytes of the text it covers.
    pub range: Range<usize>,
    /// The destination as written; resolve it only through [`MarkdownLinkTarget`].
    pub destination: String,
    /// The tooltip: the title, if any, over the destination.
    pub tooltip: String,
}

/// Inline runs as one shaped string: its text, one text run per markdown run, and the links.
#[derive(Debug, Clone)]
pub struct Inline {
    /// The visible text.
    pub text: String,
    /// One per markdown run, covering `text` in order.
    pub runs: Vec<TextRun>,
    /// The links, in order.
    pub links: Vec<InlineLink>,
}

/// The fonts and colors a stretch of inline text is drawn with.
#[derive(Debug, Clone)]
pub struct Look {
    /// The base font; code runs take [`Look::mono`].
    pub font: Font,
    /// The monospaced font.
    pub mono: Font,
    /// The text color.
    pub color: Hsla,
    /// A link's color.
    pub link: Hsla,
    /// A code span's background.
    pub code_background: Hsla,
}

/// Builds the inline text of `runs` drawn with `look`. An image stands in as a picture mark and
/// its alt text, linked to its source.
#[must_use]
pub fn inline(runs: &[MarkdownRun], look: &Look) -> Inline {
    let mut text = String::new();
    let mut out = Vec::with_capacity(runs.len());
    let mut links = Vec::new();
    for run in runs {
        let start = text.len();
        if run.is_image {
            text.push_str("\u{1F5BC}\u{FE0E} ");
        }
        text.push_str(&run.text);
        let len = text.len() - start;
        if len == 0 {
            continue;
        }
        out.push(text_run(run, len, look));
        if let Some(link) = &run.link {
            let tooltip = match &link.title {
                Some(title) => format!("{title}\n{}", link.destination),
                None => link.destination.clone(),
            };
            links.push(InlineLink {
                range: start..text.len(),
                destination: link.destination.clone(),
                tooltip,
            });
        }
    }
    Inline {
        text,
        runs: out,
        links,
    }
}

fn text_run(run: &MarkdownRun, len: usize, look: &Look) -> TextRun {
    let code = run.style.contains(MarkdownStyle::CODE);
    let mut font = if code {
        look.mono.clone()
    } else {
        look.font.clone()
    };
    if run.style.contains(MarkdownStyle::STRONG) {
        font.weight = FontWeight::BOLD;
    }
    if run.style.contains(MarkdownStyle::EMPHASIS) {
        font.style = FontStyle::Italic;
    }
    let linked = run.link.is_some();
    TextRun {
        len,
        font,
        color: if linked { look.link } else { look.color },
        background_color: code.then_some(look.code_background),
        underline: linked.then(|| UnderlineStyle {
            thickness: px(1.),
            color: Some(look.link),
            wavy: false,
        }),
        strikethrough: run.style.contains(MarkdownStyle::STRIKETHROUGH).then(|| {
            StrikethroughStyle {
                thickness: px(1.),
                color: Some(look.color),
            }
        }),
    }
}

/// What the preview asks of the pane that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewEvent {
    /// The document is parsed and on screen: a pending reveal may scroll now.
    Built,
    /// A link inside the repository was followed.
    OpenPath {
        /// The repository-relative path.
        path: String,
        /// The heading it asked for.
        fragment: Option<String>,
    },
}

/// The markdown preview of one text document.
pub struct MarkdownPreview {
    document: Arc<TextDocument>,
    source_path: Option<String>,
    rendered: Option<Arc<Rendered>>,
    list: ListState,
    /// The destination of the link under the pointer, which the context menu acts on.
    hovered: Rc<RefCell<Option<String>>>,
    focus: FocusHandle,
    _build: Task<()>,
}

impl EventEmitter<PreviewEvent> for MarkdownPreview {}

impl Focusable for MarkdownPreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MarkdownPreview {
    /// A preview of `document`, the file at `source_path` its links resolve against. The parse
    /// runs off the UI thread; dropping the preview drops the build.
    pub fn new(
        document: Arc<TextDocument>,
        source_path: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = document.text.clone();
        let build = cx.background_spawn(async move { Rendered::build(&text) });
        let task = cx.spawn(async move |this, cx| {
            let rendered = build.await;
            this.update(cx, |this, cx| this.built(rendered, cx)).ok();
        });
        Self {
            document,
            source_path,
            rendered: None,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            hovered: Rc::default(),
            focus: cx.focus_handle(),
            _build: task,
        }
    }

    fn built(&mut self, rendered: Rendered, cx: &mut Context<Self>) {
        self.list.reset(rendered.blocks.len());
        self.rendered = Some(Arc::new(rendered));
        cx.emit(PreviewEvent::Built);
        cx.notify();
    }

    /// The document on show.
    #[must_use]
    pub fn document(&self) -> &Arc<TextDocument> {
        &self.document
    }

    /// The parsed document, once built.
    #[must_use]
    pub fn rendered(&self) -> Option<&Arc<Rendered>> {
        self.rendered.as_ref()
    }

    /// The block at the top of the scroll.
    #[cfg(test)]
    pub(crate) fn top_block(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }

    /// Scrolls to the heading or footnote a `#fragment` names; `false`, leaving the scroll
    /// where it is, when nothing answers to it or the document is not built yet.
    pub fn scroll_to_anchor(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let Some(index) = self
            .rendered
            .as_ref()
            .and_then(|rendered| rendered.anchor_block(name))
        else {
            return false;
        };
        self.list.scroll_to(ListOffset {
            item_ix: index,
            offset_in_item: px(0.),
        });
        cx.notify();
        true
    }

    /// Follows one destination: a web link opens in the browser, any other scheme only after
    /// the user confirms the prompt naming it, a repository path goes up to the pane, and a
    /// bare fragment scrolls this preview. A destination that resolves to nothing is ignored.
    pub fn follow(&mut self, destination: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = MarkdownLinkTarget::resolve(destination, self.source_path.as_deref());
        match target {
            None => {}
            Some(MarkdownLinkTarget::Web(url)) => cx.open_url(&url),
            Some(MarkdownLinkTarget::OtherScheme(url)) => confirm_open(url, window, cx),
            Some(MarkdownLinkTarget::RepositoryPath { path, fragment }) => {
                cx.emit(PreviewEvent::OpenPath { path, fragment });
            }
            Some(MarkdownLinkTarget::Fragment(name)) => {
                self.scroll_to_anchor(&name, cx);
            }
        }
    }

    /// What Copy Link copies for `destination`, by [`MarkdownLinkTarget::copy_text`]; `None`
    /// for a destination that resolves to nothing.
    #[must_use]
    pub fn link_copy_text(&self, destination: &str) -> Option<String> {
        MarkdownLinkTarget::resolve(destination, self.source_path.as_deref())
            .map(|target| target.copy_text())
    }

    /// The context menu over the link under the pointer: its open command and Copy Link. No
    /// link under the pointer leaves the menu empty, and an empty menu does not open.
    fn link_menu(&self, weak: WeakEntity<Self>, menu: PopupMenu) -> PopupMenu {
        let Some(destination) = self.hovered.borrow().clone() else {
            return menu;
        };
        let Some(target) = MarkdownLinkTarget::resolve(&destination, self.source_path.as_deref())
        else {
            return menu;
        };
        let open = (weak.clone(), destination.clone());
        menu.item(
            PopupMenuItem::new(target.open_title()).on_click(move |_, window, cx| {
                let (weak, destination) = &open;
                weak.update(cx, |this, cx| this.follow(destination, window, cx))
                    .ok();
            }),
        )
        .item(PopupMenuItem::new("Copy Link").on_click(move |_, _, cx| {
            let Some(text) = weak
                .read_with(cx, |this, _| this.link_copy_text(&destination))
                .ok()
                .flatten()
            else {
                return;
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }))
    }

    fn render_block(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(rendered) = self.rendered.clone() else {
            return div().into_any_element();
        };
        let Some(block) = rendered.blocks.get(index) else {
            return div().into_any_element();
        };
        let paint = Paint::current(cx);
        let content = match &block.kind {
            BlockKind::Paragraph => self.prose(index, &block.runs, &paint.body(), cx),
            BlockKind::Heading { level } => self.heading(index, &block.runs, *level, cx),
            BlockKind::Footnote { label } => self.footnote(index, &block.runs, label, cx),
            BlockKind::Code { text, .. }
            | BlockKind::FrontMatter { text }
            | BlockKind::Html { text } => {
                code(text, rendered.fences.get(index).cloned().flatten(), &paint)
            }
            BlockKind::Table(table) => self.table(index, table, &paint, cx),
            BlockKind::ThematicBreak => rule(&paint),
        };
        let weak = cx.weak_entity();
        div()
            .id(("markdown-block", index))
            .pb(spacing_after(block))
            .child(frame(block, content, &paint))
            .context_menu(move |menu, _, cx| {
                let Some(this) = weak.upgrade() else {
                    return menu;
                };
                this.read(cx).link_menu(weak.clone(), menu)
            })
            .into_any_element()
    }

    fn heading(
        &self,
        index: usize,
        block_runs: &[MarkdownRun],
        level: u8,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let paint = &Paint::current(cx);
        let sizes = style::MARKDOWN_HEADINGS;
        let size = sizes
            .get(usize::from(level.saturating_sub(1)))
            .or(sizes.last())
            .copied()
            .unwrap_or(style::MARKDOWN_BODY);
        let mut look = paint.body();
        look.font.weight = if level <= 2 {
            FontWeight::BOLD
        } else {
            FontWeight::SEMIBOLD
        };
        if level >= 5 {
            look.color = paint.muted;
        }
        let before = if level <= 2 {
            style::MARKDOWN_MAJOR_HEADING_SPACING
        } else {
            style::MARKDOWN_HEADING_SPACING
        };
        div()
            .pt(before)
            .text_size(size)
            .child(self.prose(index, block_runs, &look, cx))
            .into_any_element()
    }

    fn footnote(
        &self,
        index: usize,
        block_runs: &[MarkdownRun],
        label: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let paint = &Paint::current(cx);
        let mut look = paint.body();
        look.color = paint.muted;
        let mut runs = vec![MarkdownRun {
            style: MarkdownStyle::STRONG,
            ..MarkdownRun::plain(format!("{label}. "))
        }];
        runs.extend(block_runs.iter().cloned());
        div()
            .text_size(style::TEXT_BODY - px(1.))
            .child(self.prose(index, &runs, &look, cx))
            .into_any_element()
    }

    /// Inline text with its links clickable, hoverable for the context menu, and tooltipped.
    fn prose(
        &self,
        id: impl Into<gpui::ElementId>,
        runs: &[MarkdownRun],
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let built = inline(runs, look);
        let text = StyledText::new(built.text).with_runs(built.runs);
        if built.links.is_empty() {
            return div().child(text).into_any_element();
        }
        let links: Rc<[InlineLink]> = built.links.into();
        let ranges = links.iter().map(|link| link.range.clone()).collect();
        let weak = cx.weak_entity();
        let hovered = Rc::clone(&self.hovered);
        let (clicked, hovering, tipped) = (Rc::clone(&links), Rc::clone(&links), links);
        InteractiveText::new(id, text)
            .on_click(ranges, move |index, window, cx| {
                if let Some(link) = clicked.get(index) {
                    let destination = link.destination.clone();
                    weak.update(cx, |this, cx| this.follow(&destination, window, cx))
                        .ok();
                }
            })
            .on_hover(move |position, _, _, _| {
                let link =
                    position.and_then(|at| hovering.iter().find(|link| link.range.contains(&at)));
                *hovered.borrow_mut() = link.map(|link| link.destination.clone());
            })
            .tooltip(move |position, window, cx| {
                let link = tipped.iter().find(|link| link.range.contains(&position))?;
                Some(Tooltip::new(link.tooltip.clone()).build(window, cx))
            })
            .into_any_element()
    }

    fn table(
        &self,
        index: usize,
        table: &MarkdownTable,
        paint: &Paint,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut grid = div()
            .flex()
            .flex_col()
            .border(style::HAIRLINE)
            .border_color(paint.border);
        let rows = std::iter::once(&table.header).chain(&table.rows);
        for (row_index, cells) in rows.enumerate() {
            let header = row_index == 0;
            let mut look = paint.body();
            if header {
                look.font.weight = FontWeight::SEMIBOLD;
            }
            let mut row = div().flex().w_full();
            if header {
                row = row.bg(paint.code_background);
            }
            for (column, cell) in cells.iter().enumerate() {
                let id = ("markdown-cell", index * 4096 + row_index * 64 + column);
                let alignment = table.alignments.get(column).copied();
                let text = self.prose(id, cell, &look, cx);
                row = row.child(aligned(cell_box(paint), alignment).child(text));
            }
            grid = grid.child(row);
        }
        grid.into_any_element()
    }
}

/// A table cell's box: an equal share of the row, bordered and padded.
fn cell_box(paint: &Paint) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .p(style::MARKDOWN_CELL_PADDING)
        .border_t(style::HAIRLINE)
        .border_r(style::HAIRLINE)
        .border_color(paint.border)
}

fn aligned(cell: gpui::Div, alignment: Option<Alignment>) -> gpui::Div {
    match alignment {
        Some(Alignment::Center) => cell.text_center(),
        Some(Alignment::Trailing) => cell.text_right(),
        Some(Alignment::Leading | Alignment::Natural) | None => cell.text_left(),
    }
}

/// A code panel: monospaced, on a faint background, colored by `spans` when there are some.
fn code(text: &str, spans: Option<Arc<[Span]>>, paint: &Paint) -> AnyElement {
    let palette = paint.palette;
    let mut styled = StyledText::new(SharedString::from(text.to_owned()));
    if let Some(spans) = spans {
        let length = text.len();
        let highlights: Vec<_> = spans
            .iter()
            .filter(|span| (span.end as usize) <= length && span.start < span.end)
            .map(|span| {
                (
                    span.start as usize..span.end as usize,
                    gpui::HighlightStyle {
                        color: Some(palette.color(span.class)),
                        ..gpui::HighlightStyle::default()
                    },
                )
            })
            .collect();
        styled = styled.with_highlights(highlights);
    }
    div()
        .w_full()
        .p(style::MARKDOWN_PANEL_PADDING)
        .rounded(style::RADIUS)
        .bg(paint.code_background)
        .font_family(style::MONOSPACE_FONT)
        .text_size(style::MARKDOWN_CODE)
        .text_color(paint.foreground)
        .child(styled)
        .into_any_element()
}

fn rule(paint: &Paint) -> AnyElement {
    div()
        .w_full()
        .pt(style::MARKDOWN_PANEL_PADDING)
        .child(div().w_full().h(style::HAIRLINE).bg(paint.border))
        .into_any_element()
}

/// The space after `block`: headings keep less after them than other blocks.
fn spacing_after(block: &MarkdownBlock) -> Pixels {
    match block.kind {
        BlockKind::Heading { .. } => px(6.),
        BlockKind::Paragraph
        | BlockKind::Code { .. }
        | BlockKind::Table(_)
        | BlockKind::ThematicBreak
        | BlockKind::FrontMatter { .. }
        | BlockKind::Html { .. }
        | BlockKind::Footnote { .. } => style::MARKDOWN_BLOCK_SPACING,
    }
}

/// `content` inside its list item and quote bars: the bars outermost, one per `>` level, then
/// the list indent, then the marker column on an item's first block.
fn frame(block: &MarkdownBlock, content: AnyElement, paint: &Paint) -> AnyElement {
    let indent = style::MARKDOWN_LIST_INDENT * list_levels(block.list_depth);
    let mut item = div().w_full().flex().pl(indent);
    if let Some(marker) = block.marker {
        item = item.child(
            div()
                .w(style::MARKDOWN_MARKER_WIDTH)
                .flex_none()
                .text_color(marker_color(marker, paint))
                .child(SharedString::from(marker_text(marker))),
        );
    } else if block.list_depth > 0 {
        item = item.pl(indent + style::MARKDOWN_MARKER_WIDTH);
    }
    let mut framed = item
        .child(div().flex_1().min_w_0().child(content))
        .into_any_element();
    for _ in 0..block.quote_depth {
        framed = div()
            .w_full()
            .border_l(style::MARKDOWN_QUOTE_BAR)
            .border_color(paint.border)
            .pl(style::MARKDOWN_QUOTE_PADDING)
            .child(framed)
            .into_any_element();
    }
    framed
}

#[expect(
    clippy::cast_precision_loss,
    reason = "list depth is capped at MAXIMUM_LIST_DEPTH, far inside f32's exact range"
)]
fn list_levels(depth: usize) -> f32 {
    depth.saturating_sub(1) as f32
}

/// What starts a list item: a bullet, its number, or a task box.
#[must_use]
pub fn marker_text(marker: MarkdownListMarker) -> String {
    match marker {
        MarkdownListMarker::Bullet => "\u{2022}".to_owned(),
        MarkdownListMarker::Ordered(value) => format!("{value}."),
        MarkdownListMarker::Task { done: true } => "\u{2611}".to_owned(),
        MarkdownListMarker::Task { done: false } => "\u{2610}".to_owned(),
    }
}

fn marker_color(marker: MarkdownListMarker, paint: &Paint) -> Hsla {
    match marker {
        MarkdownListMarker::Task { done: false } => paint.foreground,
        MarkdownListMarker::Bullet
        | MarkdownListMarker::Ordered(_)
        | MarkdownListMarker::Task { done: true } => paint.muted,
    }
}

/// Opens a link outside the web only after the user has seen the full URL and said yes: the
/// system hands it to whichever app claims the scheme, and the repository's author wrote it.
fn confirm_open(url: String, window: &mut Window, cx: &mut Context<MarkdownPreview>) {
    let detail = format!(
        "The link is not a web address. Opening it hands it to the app that claims it:\n\n\
         {url}\n\nOpen it only if you trust the repository it came from."
    );
    let answer = window.prompt(
        PromptLevel::Warning,
        "Open this link?",
        Some(&detail),
        &["Open", "Cancel"],
        cx,
    );
    cx.spawn(async move |_, cx| {
        if answer.await == Ok(0) {
            cx.update(|cx| cx.open_url(&url));
        }
    })
    .detach();
}

/// The theme's colors and fonts for one paint.
struct Paint {
    palette: Palette,
    font: Font,
    mono: Font,
    foreground: Hsla,
    muted: Hsla,
    link: Hsla,
    border: Hsla,
    code_background: Hsla,
}

impl Paint {
    fn current(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            palette: Palette::current(cx),
            font: font(theme.font_family.clone()),
            mono: font(style::MONOSPACE_FONT),
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            link: theme.link,
            border: theme.border,
            code_background: theme.foreground.opacity(style::MARKDOWN_CODE_OPACITY),
        }
    }

    fn body(&self) -> Look {
        Look {
            font: self.font.clone(),
            mono: self.mono.clone(),
            color: self.foreground,
            link: self.link,
            code_background: self.code_background,
        }
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.theme().background;
        div()
            .id("markdown-preview")
            .key_context("MarkdownPreview")
            .track_focus(&self.focus)
            .size_full()
            .bg(background)
            .text_size(style::MARKDOWN_BODY)
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, index, _window, cx| {
                        div()
                            .px(style::MARKDOWN_MARGIN)
                            .when_first(index, style::MARKDOWN_MARGIN)
                            .child(this.render_block(index, cx))
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
    }
}

/// Pads the first block from the top edge.
trait WhenFirst {
    fn when_first(self, index: usize, padding: Pixels) -> Self;
}

impl WhenFirst for gpui::Div {
    fn when_first(self, index: usize, padding: Pixels) -> Self {
        if index == 0 { self.pt(padding) } else { self }
    }
}
