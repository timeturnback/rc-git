//! The one diff view every pane draws a line diff with (`DiffRendering.swift`): History's diff
//! of a commit's file now, and the Changes pane's worktree diff next.
//!
//! # What it draws
//!
//! A [`DiffView`] shows a [`DiffContent`]: nothing, a placeholder line (a binary or too-large
//! file, a load in flight, a failure), or hunks. Hunks are laid out by
//! [`rc_git_kit::diff_rows::rows`]: each hunk's `@@` header row, then one row per line, every
//! row the same height in a `uniform_list`, so only the rows on screen are ever built and a
//! lockfile-sized diff costs what one screenful costs. A line row carries, left to right:
//!
//! 1. the caller's optional prefix element ([`DiffView::set_prefix`]);
//! 2. the old and new line numbers (an added line has no old number, a removed one no new);
//! 3. the sign and the text, colored by kind (added green, removed red, context plain) over a
//!    tint of the same color, with the line-end marker ([`line_end_marker`]) when the line did
//!    not end in a plain `\n`. Header rows are muted.
//!
//! # Building on it (the Changes pane)
//!
//! - **Content.** [`DiffContent::for_blob`] turns a core [`BlobDiff`] into hunks or the
//!   placeholder `diff_text::placeholder` words; [`DiffContent::Placeholder`] carries anything
//!   else. [`DiffView::set_content`] replaces the content and scrolls to the top.
//! - **Row prefix.** [`DiffView::set_prefix`] takes a [`RowPrefix`] closure called for every
//!   row built, with a [`RowContext`] naming the row, its [`DiffRow`] and its hunk. It returns
//!   the element to draw first in that row: Changes puts a hunk tick box on header rows and an
//!   empty spacer of the same width on line rows, so the gutters stay aligned.
//! - **Clicks.** A press on a row selects it and emits [`DiffViewEvent::RowClicked`]; a press
//!   on a header row also emits [`DiffViewEvent::HeaderClicked`] with the hunk index, which is
//!   the hook for toggling a hunk. The hunk index of any row is the one
//!   [`rc_git_kit::diff_text::owning_hunk`] names; [`DiffView::owning_hunk`] calls it, so an
//!   action on any row of a hunk acts on that hunk.
//! - **Selection and copy.** Rows select by press, shift-press and drag; ⌘C copies the selected
//!   rows as unified-diff text (header lines and signed lines) and ⌘A selects every row.
//!
//! Syntax colors are not drawn: the Swift diff had none either.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement,
    Render, ScrollStrategy, SharedString, Styled, UniformListScrollHandle, Window, div,
    prelude::FluentBuilder, uniform_list,
};
use gpui_component::ActiveTheme;
use rc_git_core::bstr::ByteSlice;
use rc_git_core::{BlobDiff, DiffHunk, DiffLine, DiffLineKind};
use rc_git_kit::diff_rows::{DiffRow, header, rows, sign};
use rc_git_kit::diff_text::{owning_hunk, placeholder};
use rc_git_kit::models::line_end_marker;

use crate::style;

/// What a diff view shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DiffContent {
    /// Nothing at all.
    #[default]
    Empty,
    /// One centered line in place of the hunks.
    Placeholder(SharedString),
    /// The hunks of a text diff.
    Hunks(Arc<[DiffHunk]>),
}

impl DiffContent {
    /// The content for a core blob diff of the file at `path`: its hunks, or the placeholder
    /// line kit words for a binary, too-large or unchanged file.
    #[must_use]
    pub fn for_blob(diff: &BlobDiff, path: &str) -> Self {
        if let Some(line) = placeholder(diff, path) {
            return Self::Placeholder(line.into());
        }
        match diff {
            BlobDiff::Text { hunks } => Self::Hunks(hunks.clone().into()),
            BlobDiff::Binary { .. } | BlobDiff::TooLarge { .. } => Self::Empty,
        }
    }
}

/// What a diff view tells its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffViewEvent {
    /// A row was pressed.
    RowClicked {
        /// The row.
        row: usize,
        /// The hunk that owns it.
        hunk: usize,
    },
    /// A hunk's header row was pressed.
    HeaderClicked {
        /// The hunk.
        hunk: usize,
    },
}

/// One row as a [`RowPrefix`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowContext {
    /// The row's index in the view.
    pub row: usize,
    /// What the row is.
    pub info: DiffRow,
}

/// Builds the element drawn first in a row; see the module documentation.
pub type RowPrefix = Rc<dyn Fn(RowContext, &mut Window, &mut App) -> AnyElement>;

/// A line diff on screen. See the module documentation.
pub struct DiffView {
    content: DiffContent,
    rows: Arc<[DiffRow]>,
    prefix: Option<RowPrefix>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    /// The selected rows: where the press began, and where it is now.
    selection: Option<(usize, usize)>,
    dragging: bool,
    /// Rows handed to the list since creation: the "build only what is on screen" budget.
    built_rows: Rc<Cell<usize>>,
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DiffView {
    /// An empty view.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            content: DiffContent::Empty,
            rows: Arc::new([]),
            prefix: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            selection: None,
            dragging: false,
            built_rows: Rc::default(),
        }
    }

    /// Shows `content`, dropping the selection and scrolling to the top. The same content
    /// again changes nothing, so a re-render of the owner keeps the scroll.
    pub fn set_content(&mut self, content: DiffContent, cx: &mut Context<Self>) {
        if content == self.content {
            return;
        }
        self.rows = match &content {
            DiffContent::Hunks(hunks) => rows(hunks).into(),
            DiffContent::Empty | DiffContent::Placeholder(_) => Arc::new([]),
        };
        self.content = content;
        self.selection = None;
        self.dragging = false;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Draws `prefix` first in every row, or nothing first when `None`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Changes pane (phase 5) builds on it; tests cover it now"
        )
    )]
    pub fn set_prefix(&mut self, prefix: Option<RowPrefix>, cx: &mut Context<Self>) {
        self.prefix = prefix;
        cx.notify();
    }

    /// The content on screen.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Changes pane (phase 5) builds on it; tests cover it now"
        )
    )]
    pub fn content(&self) -> &DiffContent {
        &self.content
    }

    /// How many rows the hunks lay out to.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Changes pane (phase 5) builds on it; tests cover it now"
        )
    )]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The hunk that owns `row`, as `diff_text::owning_hunk` maps it; `None` past the end or
    /// without hunks.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Changes pane (phase 5) builds on it; tests cover it now"
        )
    )]
    pub fn owning_hunk(&self, row: usize) -> Option<&DiffHunk> {
        owning_hunk(row, self.hunks())
    }

    /// The selected rows, first to last.
    #[must_use]
    pub fn selected_rows(&self) -> Option<Range<usize>> {
        let (anchor, head) = self.selection?;
        Some(anchor.min(head)..anchor.max(head) + 1)
    }

    /// The selected rows as unified-diff text, one line each.
    #[must_use]
    pub fn selected_text(&self) -> String {
        let Some(range) = self.selected_rows() else {
            return String::new();
        };
        let mut text = String::new();
        for row in range {
            if let Some(line) = self.row_text(row) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        text
    }

    /// Copies the selected rows; nothing when none are.
    pub fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Selects every row.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        self.selection = Some((0, self.rows.len() - 1));
        cx.notify();
    }

    /// How many rows have been built since the view was created.
    #[cfg(test)]
    pub(crate) fn built_rows(&self) -> usize {
        self.built_rows.get()
    }

    fn hunks(&self) -> &[DiffHunk] {
        match &self.content {
            DiffContent::Hunks(hunks) => hunks,
            DiffContent::Empty | DiffContent::Placeholder(_) => &[],
        }
    }

    fn line(&self, hunk: usize, line: usize) -> Option<&DiffLine> {
        self.hunks().get(hunk)?.lines.get(line)
    }

    /// The text of `row` as a unified diff prints it, without a line-end marker.
    fn row_text(&self, row: usize) -> Option<String> {
        match *self.rows.get(row)? {
            DiffRow::Header { hunk } => Some(header(self.hunks().get(hunk)?)),
            DiffRow::Line { hunk, line, .. } => {
                let line = self.line(hunk, line)?;
                Some(format!("{}{}", sign(line.kind), line.text.to_str_lossy()))
            }
        }
    }

    fn press(&mut self, row: usize, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(info) = self.rows.get(row).copied() else {
            return;
        };
        self.selection = match self.selection {
            Some((anchor, _)) if event.modifiers.shift => Some((anchor, row)),
            Some(_) | None => Some((row, row)),
        };
        self.dragging = true;
        cx.emit(DiffViewEvent::RowClicked {
            row,
            hunk: info.hunk(),
        });
        if let DiffRow::Header { hunk } = info {
            cx.emit(DiffViewEvent::HeaderClicked { hunk });
        }
        cx.notify();
    }

    fn dragged(&mut self, row: usize, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.dragging || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some((anchor, head)) = self.selection
            && head != row
        {
            self.selection = Some((anchor, row));
            cx.notify();
        }
    }

    fn render_row(&self, row: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(info) = self.rows.get(row).copied() else {
            return div().into_any_element();
        };
        let colors = RowColors::current(cx);
        let selected = self
            .selected_rows()
            .is_some_and(|range| range.contains(&row));
        let prefix = self
            .prefix
            .as_ref()
            .map(|prefix| prefix(RowContext { row, info }, window, cx));
        let (old, new, body, tint) = match info {
            DiffRow::Header { hunk } => {
                let text = self.hunks().get(hunk).map(header).unwrap_or_default();
                (None, None, header_body(text, colors.muted), None)
            }
            DiffRow::Line {
                hunk,
                line,
                old,
                new,
            } => {
                let Some(line) = self.line(hunk, line) else {
                    return div().into_any_element();
                };
                let color = colors.of(line.kind);
                let tint = (line.kind != DiffLineKind::Context).then_some(color);
                (old, new, line_body(line, color, colors.muted), tint)
            }
        };
        div()
            .id(row)
            .flex()
            .flex_none()
            .w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .font_family(style::MONOSPACE_FONT)
            .text_size(style::TEXT_MONOSPACE)
            .when_some(tint, |div, tint| {
                div.bg(tint.opacity(style::DIFF_TINT_OPACITY))
            })
            .when(selected, |div| {
                div.bg(colors.selection.opacity(style::SELECTION_OPACITY))
            })
            .children(prefix)
            .child(number(old, colors.muted))
            .child(number(new, colors.muted))
            .child(body)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| this.press(row, event, cx)),
            )
            .on_mouse_move(cx.listener(move |this, event, _, cx| this.dragged(row, event, cx)))
            .into_any_element()
    }

    fn render_placeholder(text: SharedString, cx: &Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px(style::MARGIN)
            .text_size(style::TEXT_BODY)
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }
}

/// The diff colors of the active theme.
#[derive(Debug, Clone, Copy)]
struct RowColors {
    foreground: Hsla,
    muted: Hsla,
    added: Hsla,
    removed: Hsla,
    selection: Hsla,
}

impl RowColors {
    fn current(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            foreground: theme.foreground,
            muted: theme.muted_foreground,
            added: theme.green,
            removed: theme.red,
            selection: theme.selection,
        }
    }

    fn of(&self, kind: DiffLineKind) -> Hsla {
        match kind {
            DiffLineKind::Context => self.foreground,
            DiffLineKind::Added => self.added,
            DiffLineKind::Removed => self.removed,
        }
    }
}

fn number(value: Option<u32>, color: Hsla) -> impl IntoElement {
    div()
        .w(style::DIFF_NUMBER_WIDTH)
        .flex_none()
        .flex()
        .justify_end()
        .pr(style::ROW_GAP)
        .text_color(color)
        .children(value.map(|value| SharedString::from(value.to_string())))
}

fn header_body(text: String, color: Hsla) -> AnyElement {
    div()
        .pl(style::ROW_GAP)
        .text_color(color)
        .child(SharedString::from(text))
        .into_any_element()
}

fn line_body(line: &DiffLine, color: Hsla, muted: Hsla) -> AnyElement {
    let text = format!("{}{}", sign(line.kind), line.text.to_str_lossy());
    div()
        .flex()
        .pl(style::ROW_GAP)
        .child(div().text_color(color).child(SharedString::from(text)))
        .children(
            line_end_marker(line.eol)
                .map(|marker| div().text_color(muted).child(SharedString::from(marker))),
        )
        .into_any_element()
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.content {
            DiffContent::Empty => div().into_any_element(),
            DiffContent::Placeholder(text) => Self::render_placeholder(text.clone(), cx),
            DiffContent::Hunks(_) => uniform_list(
                "diff-rows",
                self.rows.len(),
                cx.processor(move |this, range: Range<usize>, window, cx| {
                    this.built_rows.set(this.built_rows.get() + range.len());
                    let mut rows = Vec::with_capacity(range.len());
                    for row in range {
                        rows.push(this.render_row(row, window, cx));
                    }
                    rows
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element(),
        };
        div()
            .id("diff-view")
            .key_context("DiffView")
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &gpui_component::input::Copy, _, cx| this.copy(cx)))
            .on_action(
                cx.listener(|this, _: &gpui_component::input::SelectAll, _, cx| {
                    this.select_all(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = false),
            )
            .child(body)
    }
}
