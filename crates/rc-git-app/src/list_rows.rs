//! The long-line mode shared by the views that draw text rows (the diff view and the file
//! viewer): [`WrapLines`] picks it app-wide, and [`RowScroll`] holds both lists' scroll so a
//! view switches between them and keeps its top row in view.
//!
//! Wrapping, rows wrap to the pane's width in a variable-height `list`, which still lays out
//! only the rows on screen. One-line, rows stay one line high in a `uniform_list` that scrolls
//! sideways as far as the widest row, with a scrollbar on each axis.

use gpui::{
    App, Global, ListAlignment, ListOffset, ListState, Pixels, ScrollStrategy,
    UniformListScrollHandle, px,
};
use gpui_component::scroll::Scrollbar;

/// Whether every diff view and file viewer wraps long lines. The shell sets it from the saved
/// preference; absent reads as on, the preference's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapLines(pub bool);

impl Global for WrapLines {}

impl WrapLines {
    /// Whether lines wrap now.
    #[must_use]
    pub fn on(cx: &App) -> bool {
        cx.try_global::<Self>().is_none_or(|wrap| wrap.0)
    }
}

/// How far past the visible rows the wrapping list lays out, so a scroll step finds rows
/// already measured.
const WRAPPED_OVERDRAW: Pixels = px(200.);

/// The scroll of a view's rows in both modes, and which one drew last.
pub struct RowScroll {
    /// The one-line list's scroll, used while [`WrapLines`] is off.
    one_line: UniformListScrollHandle,
    /// The wrapping list's rows and scroll, used while [`WrapLines`] is on.
    wrapped: ListState,
    /// Whether the last frame wrapped, so a switch can carry the top row over.
    wraps: bool,
    count: usize,
}

impl RowScroll {
    /// `count` rows at the top, in the mode [`WrapLines`] picks now.
    pub fn new(count: usize, cx: &App) -> Self {
        Self {
            one_line: UniformListScrollHandle::new(),
            wrapped: ListState::new(count, ListAlignment::Top, WRAPPED_OVERDRAW),
            wraps: WrapLines::on(cx),
            count,
        }
    }

    /// Whether the rows draw wrapped.
    pub fn wraps(&self) -> bool {
        self.wraps
    }

    /// The one-line list's scroll, for `track_scroll`.
    pub fn one_line(&self) -> &UniformListScrollHandle {
        &self.one_line
    }

    /// The wrapping list's state, for `list`.
    pub fn wrapped(&self) -> &ListState {
        &self.wrapped
    }

    /// `count` new rows, both lists back at the top and the left edge.
    pub fn reset(&mut self, count: usize) {
        self.count = count;
        self.one_line
            .0
            .borrow()
            .base_handle
            .set_offset(gpui::Point::default());
        self.one_line.scroll_to_item(0, ScrollStrategy::Top);
        self.wrapped.reset(count);
    }

    /// Draws wrapped rows when `wraps`, one-line rows otherwise, keeping the top row in view.
    pub fn follow(&mut self, wraps: bool) {
        if wraps == self.wraps {
            return;
        }
        let top = self.top_row();
        self.wraps = wraps;
        self.scroll_to_row(top);
    }

    /// Scrolls `row` to the top of whichever list draws.
    pub fn scroll_to_row(&self, row: usize) {
        if self.wraps {
            self.wrapped.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: Pixels::ZERO,
            });
        } else {
            self.one_line.scroll_to_item(row, ScrollStrategy::Top);
        }
    }

    /// Brings `row` into view: centered in the one-line list, just revealed in the wrapping
    /// one, which cannot center a row it has not measured.
    pub fn reveal(&self, row: usize) {
        if self.wraps {
            self.wrapped.scroll_to_reveal_item(row);
        } else {
            self.one_line.scroll_to_item(row, ScrollStrategy::Center);
        }
    }

    /// The top row on screen, in whichever list drew last.
    pub fn top_row(&self) -> usize {
        if self.wraps {
            return self.wrapped.logical_scroll_top().item_ix;
        }
        one_line_top_row(&self.one_line, self.count)
    }

    /// The scrollbar for whichever list draws: vertical over wrapped rows, both axes over
    /// one-line rows.
    pub fn scrollbar(&self) -> Scrollbar {
        if self.wraps {
            Scrollbar::vertical(&self.wrapped)
        } else {
            Scrollbar::new(&self.one_line)
        }
    }

    /// The one-line list's content width over its viewport width, from the last frame.
    #[cfg(test)]
    pub fn scroll_widths(&self) -> Option<(Pixels, Pixels)> {
        let size = self.one_line.0.borrow().last_item_size?;
        Some((size.contents.width, size.item.width))
    }

    /// How far the one-line list is scrolled.
    #[cfg(test)]
    pub fn scroll_offset(&self) -> gpui::Point<Pixels> {
        self.one_line.0.borrow().base_handle.offset()
    }

    /// The height `row` was laid out at in the wrapping list, once it was.
    #[cfg(test)]
    pub fn wrapped_row_height(&self, row: usize) -> Option<Pixels> {
        Some(self.wrapped.bounds_for_item(row)?.size.height)
    }
}

/// The top row on screen in the `uniform_list` `scroll` tracks, over `count` rows; 0 before
/// the list has drawn.
fn one_line_top_row(scroll: &UniformListScrollHandle, count: usize) -> usize {
    let state = scroll.0.borrow();
    let Some(size) = state.last_item_size else {
        return 0;
    };
    if count == 0 || size.contents.height <= Pixels::ZERO {
        return 0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a row count past 2^24 only blurs the height of one row by a hair"
    )]
    let row_height = size.contents.height / count as f32;
    // The list floors the same quotient to find its first row; the hair over keeps a row
    // scrolled exactly to the top from reading as the one above it.
    let rows_above = (-state.base_handle.offset().y / row_height + 1e-3).max(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative row offset floors to a row index; the min keeps it in range"
    )]
    let row = rows_above.floor() as usize;
    row.min(count - 1)
}
