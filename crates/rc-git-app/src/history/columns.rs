//! The commit list's header and its author and date column widths. A handle on the left edge
//! of each of those header cells drags that column's width; the message column takes what
//! is left. The widths follow `rc_git_kit::layout_policy`: the date opens fitted to the widest
//! date, the author narrow, and a drag saves the width it ends on.

use gpui::{
    AnyElement, App, AppContext, Context, DragMoveEvent, Empty, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Render, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder,
};
use gpui_component::ActiveTheme;
use rc_git_kit::dates::list_date_time_samples;
use rc_git_kit::layout_policy::{
    HistoryColumn, HistoryColumns, SavedHistoryColumns, dragged_history_column, history_columns,
};
use rc_git_kit::platform::stores::{HistoryColumnsStore, SharedSettings};

use super::{HistoryPane, graph};
use crate::files::fit::shaped_width;
use crate::log;
use crate::style::{self, points, value};

/// What a column handle drags; it draws nothing under the pointer. The column is the drag
/// origin's.
#[derive(Debug, Clone, Copy)]
pub struct ColumnDrag;

impl Render for ColumnDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Where a drag started: the column, the pointer and the column's width then, and whether
/// the pointer has moved the width since.
#[derive(Debug, Clone, Copy)]
struct DragOrigin {
    column: HistoryColumn,
    x: Pixels,
    width: f64,
    moved: bool,
}

/// The author and date widths: as saved or being dragged, and the date's fitted width.
pub struct ColumnWidths {
    store: HistoryColumnsStore,
    saved: SavedHistoryColumns,
    date_fit: Option<f64>,
    drag: Option<DragOrigin>,
}

impl ColumnWidths {
    /// The widths as last saved.
    pub fn load(settings: SharedSettings) -> Self {
        let store = HistoryColumnsStore::new(settings);
        Self {
            saved: store.load(),
            store,
            date_fit: None,
            drag: None,
        }
    }

    /// Measures the widest date the list can show, once.
    pub fn fit_date(&mut self, window: &Window, cx: &App) {
        if self.date_fit.is_some() {
            return;
        }
        let family = cx.theme().font_family.clone();
        let mut widest = Pixels::ZERO;
        for sample in list_date_time_samples() {
            widest = widest.max(shaped_width(
                &sample,
                &family,
                style::TEXT_CAPTION,
                window,
                cx,
            ));
        }
        self.date_fit = Some(value(widest).ceil() + value(style::COLUMN_FIT_SLACK));
    }

    /// The widths the columns are drawn at now.
    pub fn current(&self) -> HistoryColumns {
        history_columns(self.saved, self.date_fit.unwrap_or(0.0))
    }

    fn start(&mut self, column: HistoryColumn, x: Pixels) {
        self.drag = Some(DragOrigin {
            column,
            x,
            width: self.current().width(column),
            moved: false,
        });
    }

    /// Moves the dragged column's edge to the pointer at `x`, inside the `room` the message,
    /// author and date columns share. `true` when a width changed.
    fn drag_to(&mut self, x: Pixels, room: f64) -> bool {
        let Some(origin) = self.drag else {
            return false;
        };
        let asked = origin.width - value(x - origin.x);
        let width = dragged_history_column(self.current(), origin.column, asked, room);
        let slot = match origin.column {
            HistoryColumn::Author => &mut self.saved.author,
            HistoryColumn::Date => &mut self.saved.date,
        };
        if *slot == Some(width) {
            return false;
        }
        *slot = Some(width);
        if let Some(origin) = &mut self.drag {
            origin.moved = true;
        }
        true
    }

    /// Ends a drag, saving the width it left. A press that moved nothing saves nothing, so a
    /// column never dragged keeps following its default.
    fn finish(&mut self) {
        let Some(origin) = self.drag.take() else {
            return;
        };
        if !origin.moved {
            return;
        }
        let width = self.current().width(origin.column);
        if let Err(error) = self.store.save(origin.column, width) {
            log::error(&error.to_string());
        }
    }
}

impl HistoryPane {
    /// The width of the row outside the message, author and date cells: the graph cell and its
    /// gap (or the left margin, with no graph), the hash, the three gaps between the hash and
    /// the date, and the right margin.
    fn leading_width(&self, cx: &App) -> Pixels {
        let model = self.model.read(cx);
        let lead = if model.graph_rows().is_empty() {
            style::MARGIN
        } else {
            graph::width(model.graph_max_lane_count()) + style::ROW_GAP
        };
        lead + style::HASH_COLUMN_WIDTH + style::ROW_GAP * 3. + style::MARGIN
    }

    /// The header over the commit rows: a label per column, with a drag handle on the left
    /// edge of the author and date cells.
    pub(super) fn render_column_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let graphed = !model.graph_rows().is_empty();
        let lanes = model.graph_max_lane_count();
        let columns = self.columns.current();
        let theme = cx.theme();
        div()
            .id("commit-header")
            .flex()
            .flex_none()
            .items_center()
            .gap(style::ROW_GAP)
            .w_full()
            .h(style::LIST_ROW_HEIGHT)
            .pr(style::MARGIN)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(style::TEXT_CAPTION)
            .text_color(theme.muted_foreground)
            .border_b(style::HAIRLINE)
            .border_color(theme.border)
            .when(graphed, |header| {
                header.child(graph::empty(graph::width(lanes)))
            })
            .when(!graphed, |header| header.pl(style::MARGIN))
            .child(
                div()
                    .w(style::HASH_COLUMN_WIDTH)
                    .flex_none()
                    .child("Commit"),
            )
            .child(div().flex_1().min_w_0().child("Message"))
            .child(Self::header_cell(
                HistoryColumn::Author,
                "Author",
                columns,
                cx,
            ))
            .child(Self::header_cell(HistoryColumn::Date, "Date", columns, cx))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ColumnDrag>, _, cx| {
                    let room = value(event.bounds.size.width - this.leading_width(cx));
                    if this.columns.drag_to(event.event.position.x, room) {
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    fn header_cell(
        column: HistoryColumn,
        label: &'static str,
        columns: HistoryColumns,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let line = cx.theme().border;
        let (id, cell) = match column {
            HistoryColumn::Author => ("author-column-handle", "author-column"),
            HistoryColumn::Date => ("date-column-handle", "date-column"),
        };
        let handle = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .absolute()
            .top_0()
            .bottom_0()
            .left(-(style::ROW_GAP + style::COLUMN_HANDLE_WIDTH) / 2.)
            .w(style::COLUMN_HANDLE_WIDTH)
            .flex()
            .justify_center()
            .cursor_col_resize()
            .child(div().w(style::HAIRLINE).h_full().bg(line))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.columns.start(column, event.position.x);
                }),
            )
            .on_drag(ColumnDrag, |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| *drag)
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.columns.finish()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.columns.finish()),
            );
        div()
            .debug_selector(move || cell.to_owned())
            .relative()
            .w(points(columns.width(column)))
            .flex_none()
            .child(div().overflow_hidden().text_ellipsis().child(label))
            .child(handle)
    }
}
