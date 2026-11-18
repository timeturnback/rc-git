//! Find in Repository (`FindInRepositoryController`, ⇧⌘F in Files): type a literal string,
//! press Return, and every file below the root is searched in the background, hits listing as
//! they land, grouped by file. Return again, or a double-click, opens the highlighted line at
//! its match. Kit's [`ContentSearchSession`] runs the query over the browser model's
//! `search_content`; the session keeps its results while the panel is closed, until the browsed
//! root changes.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, InteractiveElement, IntoElement, ParentElement, Render, ScrollStrategy,
    SharedString, Stateful, StatefulInteractiveElement, Styled, StyledText, Subscription,
    UniformListScrollHandle, Window, div, uniform_list,
};
use gpui_component::ActiveTheme;
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::{Input, InputState};
use rc_git_kit::browser::{BrowserModel, ContentSearchError};
use rc_git_kit::content_search::{
    ContentLineMatch, ContentReport, ContentSearchEvent, ContentSearchHit, ContentSearchProvider,
    ContentSearchRow, ContentSearchSession, ContentSearchStatus, status_text,
};
use rc_git_kit::path_search::PathIndexEntry;
use rc_git_kit::picker;
use rc_git_kit::repository::RepositoryError;

use crate::icons::entry_glyph;
use crate::search::{PanelKey, body, field, frame, keys, note, row};
use crate::style;

/// What the panel tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindEvent {
    /// Open this file, at this match when one is given.
    Open(PathIndexEntry, Option<ContentLineMatch>),
    /// Escape, the close button or a click outside: the panel hides; a search keeps running.
    Dismissed,
}

/// The Find in Repository panel of one window.
pub struct FindPanel {
    session: Entity<ContentSearchSession>,
    input: Entity<InputState>,
    highlight: Option<usize>,
    /// The root the results were searched under; another one empties the list.
    scope: Option<String>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FindEvent> for FindPanel {}

impl Focusable for FindPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl FindPanel {
    /// A panel searching `model`'s file contents.
    pub fn new(model: Entity<BrowserModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let provider: ContentSearchProvider =
            Rc::new(move |query: String, report: ContentReport, cx: &mut App| {
                let search = model.update(cx, |model, cx| model.search_content(&query, report, cx));
                cx.spawn(async move |_| {
                    search.await.map_err(|error| match error {
                        ContentSearchError::Failed(error) => error,
                        ContentSearchError::Cancelled => {
                            RepositoryError::new("content search: a newer search replaced it")
                        }
                    })
                })
            });
        let session = cx.new(|_| {
            let mut session = ContentSearchSession::new();
            session.set_provider(provider);
            session
        });
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Find in repository \u{2014} Return to search")
        });
        let subscriptions = vec![cx.subscribe(
            &session,
            |this, _, event: &ContentSearchEvent, cx| match event {
                ContentSearchEvent::Updated => this.updated(cx),
                ContentSearchEvent::Open { entry, line } => {
                    cx.emit(FindEvent::Open(entry.clone(), line.clone()));
                }
            },
        )];
        Self {
            session,
            input,
            highlight: None,
            scope: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The session, for tests.
    #[cfg(test)]
    pub(crate) fn session(&self) -> &Entity<ContentSearchSession> {
        &self.session
    }

    /// The highlighted row.
    #[cfg(test)]
    pub(crate) fn highlight(&self) -> Option<usize> {
        self.highlight
    }

    /// Opens the panel over `scope`, the root being browsed, with the last query selected;
    /// results searched under another root are dropped.
    pub fn show(&mut self, scope: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.scope.as_ref() != Some(&scope) {
            self.scope = Some(scope);
            self.highlight = None;
            self.session.update(cx, ContentSearchSession::reset);
        }
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// One of the keys the field hands over.
    pub fn key(&mut self, key: PanelKey, cx: &mut Context<Self>) {
        match key {
            PanelKey::Up => self.move_highlight(-1, cx),
            PanelKey::Down => self.move_highlight(1, cx),
            PanelKey::Enter => {
                let query = self.input.read(cx).value().to_string();
                let selected = self.highlight;
                self.session
                    .update(cx, |session, cx| session.submit(&query, selected, cx));
            }
            PanelKey::Escape => cx.emit(FindEvent::Dismissed),
        }
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.session.read(cx).rows().len();
        self.highlight = picker::moved(self.highlight, delta, count);
        if let Some(row) = self.highlight {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn updated(&mut self, cx: &mut Context<Self>) {
        let count = self.session.read(cx).rows().len();
        self.highlight = picker::kept_highlight(self.highlight, count);
        cx.notify();
    }

    fn clicked(&mut self, index: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        self.highlight = Some(index);
        if event.click_count() >= 2 {
            self.session
                .update(cx, |session, cx| session.open(index, cx));
        }
        cx.notify();
    }

    fn status(&self, cx: &App) -> Option<String> {
        match self.session.read(cx).status() {
            ContentSearchStatus::Idle => None,
            ContentSearchStatus::Searching(progress) => Some(status_text(&progress, false)),
            ContentSearchStatus::Finished(progress) => Some(status_text(&progress, true)),
            ContentSearchStatus::Failed(message) => Some(format!("Cannot search: {message}")),
        }
    }

    fn rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<Stateful<gpui::Div>> {
        let session = self.session.read(cx);
        let picked: Vec<(usize, ContentSearchRow)> = range
            .filter_map(|index| session.rows().get(index).map(|row| (index, *row)))
            .collect();
        let mut out = Vec::with_capacity(picked.len());
        for (index, kind) in picked {
            let highlighted = self.highlight == Some(index);
            let element = match kind {
                ContentSearchRow::File(file) => {
                    let hit = self.session.read(cx).hits().get(file).cloned();
                    hit.map(|hit| file_row(index, &hit, highlighted, cx))
                }
                ContentSearchRow::Line { file, line } => {
                    let found = self
                        .session
                        .read(cx)
                        .hits()
                        .get(file)
                        .and_then(|hit| hit.lines.get(line).cloned());
                    found.map(|found| line_row(index, &found, highlighted, cx))
                }
            };
            let Some(element) = element else {
                continue;
            };
            out.push(
                element.on_click(
                    cx.listener(move |this, event, _, cx| this.clicked(index, event, cx)),
                ),
            );
        }
        out
    }
}

/// The file's name, then its path and how many lines matched, dimmed.
fn file_row(
    index: usize,
    hit: &ContentSearchHit,
    highlighted: bool,
    cx: &App,
) -> Stateful<gpui::Div> {
    let count = hit.lines.len();
    let more = if hit.truncated { "+" } else { "" };
    let plural = if count == 1 { "" } else { "s" };
    let detail = format!("{} \u{b7} {count}{more} line{plural}", hit.entry.path);
    let glyph = entry_glyph(hit.entry.kind, hit.entry.name());
    let name = SharedString::from(hit.entry.name().to_owned());
    row(
        ("hit", index),
        Some(glyph),
        (name, SharedString::from(detail)),
        highlighted,
        cx,
    )
    .font_weight(gpui::FontWeight::MEDIUM)
}

/// The 1-based line number, dimmed, then the excerpt with each match marked.
fn line_row(
    index: usize,
    found: &ContentLineMatch,
    highlighted: bool,
    cx: &App,
) -> Stateful<gpui::Div> {
    let theme = cx.theme();
    let marks: Vec<(Range<usize>, HighlightStyle)> = found
        .highlights
        .iter()
        .filter(|range| range.end <= found.excerpt.len())
        .map(|range| {
            (
                range.clone(),
                HighlightStyle {
                    background_color: Some(theme.yellow.opacity(style::SELECTION_OPACITY)),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    let excerpt = StyledText::new(found.excerpt.clone()).with_highlights(marks);
    let mut element = div()
        .id(("hit", index))
        .h(style::PANEL_ROW_HEIGHT)
        .flex()
        .items_center()
        .gap(style::ROW_GAP)
        .pl(style::ICON_SIZE + style::ROW_GAP * 2.)
        .rounded(style::RADIUS)
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(style::TEXT_CAPTION)
        .hover(|row| row.bg(theme.list_hover));
    if highlighted {
        element = element.bg(theme.list_active);
    }
    element
        .child(
            div()
                .flex_none()
                .text_color(theme.muted_foreground)
                .child(SharedString::from((found.line + 1).to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .font_family(style::MONOSPACE_FONT)
                .child(excerpt),
        )
}

impl Render for FindPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.session.read(cx).rows().len();
        let status = self.status(cx).map(|text| note(text, cx));
        let close = gpui_component::button::Button::new("find-close")
            .ghost()
            .label("\u{2715}")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(FindEvent::Dismissed)));
        let content = div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                field(
                    div()
                        .flex_1()
                        .child(Input::new(&self.input).appearance(false)),
                    cx,
                )
                .child(close),
            )
            .child(
                body()
                    .child(
                        uniform_list(
                            "find-results",
                            count,
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                this.rows(range, cx)
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .flex_1(),
                    )
                    .children(status),
            );
        let root = frame(
            "find-in-repository",
            (style::FIND_PANEL_WIDTH, style::FIND_PANEL_HEIGHT),
            content,
            cx,
        )
        .key_context("FindInRepository")
        .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(FindEvent::Dismissed)));
        keys(root, cx, |this, key, _, cx| this.key(key, cx))
    }
}
