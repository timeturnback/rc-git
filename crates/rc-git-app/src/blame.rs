//! Blame in a second window (`BlamePanelController`): which commit introduced each run of lines
//! of the open file. The window follows the model's [`BlameState`]: a walk in flight opens it
//! empty under the "Blaming …" title, a finished walk fills it, an empty state closes it. A
//! window already on screen stays where it is; a closed one opens again on the next blame.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render,
    SharedString, Styled, TitlebarOptions, UniformListScrollHandle, Window, WindowBounds,
    WindowOptions, div, uniform_list,
};
use gpui_component::scroll::Scrollbar;
use gpui_component::{ActiveTheme, Root};
use rc_git_kit::history_text::{blame_date, blame_lines, short_hash};
use rc_git_kit::models::{BlameSpan, BlameState};

use crate::history::search::OffsetAt;
use crate::log;
use crate::style;

/// The table inside the blame window.
pub struct BlameView {
    spans: Rc<[BlameSpan]>,
    offset_at: OffsetAt,
    scroll: UniformListScrollHandle,
}

impl BlameView {
    fn new(offset_at: OffsetAt) -> Self {
        Self {
            spans: Rc::new([]),
            offset_at,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// The ranges on show.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn spans(&self) -> &[BlameSpan] {
        &self.spans
    }

    fn render_row(&self, index: usize, cx: &App) -> gpui::AnyElement {
        let Some(span) = self.spans.get(index) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let range = &span.range;
        let date = blame_date(span.older_than, span.commit.author_time, self.offset_at);
        let cell = |width: Option<gpui::Pixels>, text: String| {
            let cell = div()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(SharedString::from(text));
            match width {
                Some(width) => cell.w(width).flex_none(),
                None => cell.flex_1().min_w_0(),
            }
        };
        div()
            .flex()
            .items_center()
            .gap(style::ROW_GAP)
            .h(style::LIST_ROW_HEIGHT)
            .px(style::MARGIN)
            .text_size(style::TEXT_CAPTION)
            .text_color(theme.foreground)
            .child(cell(
                Some(style::BLAME_LINES_WIDTH),
                blame_lines(range.start_line, range.line_count),
            ))
            .child(
                cell(Some(style::HASH_COLUMN_WIDTH), short_hash(&span.commit.oid))
                    .font_family(style::MONOSPACE_FONT)
                    .text_color(theme.muted_foreground),
            )
            .child(cell(
                Some(style::BLAME_AUTHOR_WIDTH),
                span.commit.author_name.clone(),
            ))
            .child(cell(Some(style::BLAME_DATE_WIDTH), date))
            .child(cell(None, span.commit.title.clone()))
            .into_any_element()
    }

    fn render_header(cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let cell = |width: Option<gpui::Pixels>, text: &'static str| {
            let cell = div().child(text);
            match width {
                Some(width) => cell.w(width).flex_none(),
                None => cell.flex_1(),
            }
        };
        div()
            .flex()
            .gap(style::ROW_GAP)
            .px(style::MARGIN)
            .py(style::ROW_GAP)
            .border_b(style::HAIRLINE)
            .border_color(theme.border)
            .text_size(style::TEXT_CAPTION)
            .text_color(theme.muted_foreground)
            .child(cell(Some(style::BLAME_LINES_WIDTH), "Lines"))
            .child(cell(Some(style::HASH_COLUMN_WIDTH), "Commit"))
            .child(cell(Some(style::BLAME_AUTHOR_WIDTH), "Author"))
            .child(cell(Some(style::BLAME_DATE_WIDTH), "Date"))
            .child(cell(None, "Title"))
    }
}

impl Render for BlameView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(Self::render_header(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        uniform_list(
                            "blame-rows",
                            self.spans.len(),
                            cx.processor(|this, range: Range<usize>, _window, cx| {
                                let mut rows = Vec::with_capacity(range.len());
                                for index in range {
                                    rows.push(this.render_row(index, cx));
                                }
                                rows
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.scroll)),
            )
    }
}

/// The blame window of one repository window, while it is open.
pub struct BlamePanel {
    offset_at: OffsetAt,
    open: Option<(AnyWindowHandle, Entity<BlameView>)>,
}

impl BlamePanel {
    /// No window yet.
    #[must_use]
    pub fn new(offset_at: OffsetAt) -> Self {
        Self {
            offset_at,
            open: None,
        }
    }

    /// The view while the window is open, for tests.
    #[cfg(test)]
    pub(crate) fn view(&self) -> Option<&Entity<BlameView>> {
        self.open.as_ref().map(|(_, view)| view)
    }

    /// Applies `state`: shows, fills or closes the window. A failure closes it and hands the
    /// message back for the caller to present.
    pub fn render(&mut self, state: &BlameState, cx: &mut App) -> Option<String> {
        match state {
            BlameState::Empty => self.close(cx),
            BlameState::Loading { title, .. } => self.show(Rc::new([]), title, cx),
            BlameState::Loaded { spans, title, .. } => self.show(spans.clone().into(), title, cx),
            BlameState::Failed(message) => {
                self.close(cx);
                return Some(message.clone());
            }
        }
        None
    }

    /// Closes the window.
    pub fn close(&mut self, cx: &mut App) {
        if let Some((handle, _)) = self.open.take() {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
        }
    }

    fn show(&mut self, spans: Rc<[BlameSpan]>, title: &str, cx: &mut App) {
        if let Some((handle, view)) = &self.open {
            let shown = handle.update(cx, |_, window, cx| {
                window.set_window_title(title);
                view.update(cx, |view, cx| {
                    view.spans = Rc::clone(&spans);
                    cx.notify();
                });
                window.activate_window();
            });
            if shown.is_ok() {
                return;
            }
        }
        self.open = None;
        let offset_at = self.offset_at;
        let size = gpui::size(style::BLAME_WINDOW_WIDTH, style::BLAME_WINDOW_HEIGHT);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(title.to_owned())),
                ..TitlebarOptions::default()
            }),
            ..WindowOptions::default()
        };
        let mut made = None;
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|_| BlameView {
                spans,
                ..BlameView::new(offset_at)
            });
            made = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        });
        match (opened, made) {
            (Ok(handle), Some(view)) => self.open = Some((handle.into(), view)),
            (Err(error), _) => log::error(&format!("open the blame window: {error:#}")),
            (Ok(_), None) => {}
        }
    }
}
