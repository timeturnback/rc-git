//! The Changes mode (`ChangesViewController.swift`), split across the window as Swift split
//! it: the sidebar section under the banner ([`list::ChangeListView`]: the sync row, the
//! change list with its tick boxes, the stash list and the commit box) and the content pane
//! ([`pane::WorktreeDiffPane`]: one file's diff against `HEAD` with a tick box per hunk).
//!
//! Both render the window's `ChangesModel` and forward every mutating request out as a
//! [`ChangesRequest`]; the window runs it through the guard. Neither classifies or performs
//! an action itself.

pub mod list;
pub mod pane;
mod rows;
mod stash;

use gpui::{App, Hsla, InteractiveElement, IntoElement, ParentElement, Styled, div};
use gpui_component::{ActiveTheme, Icon};
use rc_git_core::StashEntry;
use rc_git_kit::changes::TickState;
use rc_git_kit::guard_bridge::HunkStamp;

use crate::icons::{Glyph, icon};
use crate::style;

/// A mutating request from the Changes views, for the window to run through the guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangesRequest {
    /// Commit the ticked selection with this message.
    Commit(String),
    /// Stash the ticked files, with the commit box's text as the message when it has one.
    StashPush(Option<String>),
    /// Apply this stash, keeping it.
    StashApply(StashEntry),
    /// Apply this stash and drop it.
    StashPop(StashEntry),
    /// Drop this stash.
    StashDrop(StashEntry),
    /// Discard every change in these files.
    Discard(Vec<String>),
    /// Discard one hunk of a file.
    DiscardHunk {
        /// The file.
        path: String,
        /// The hunk.
        stamp: HunkStamp,
    },
    /// The sync row's Fetch.
    Fetch,
    /// The sync row's Pull.
    Pull,
    /// The sync row's Push.
    Push,
}

/// A tick box of `state`, dimmed when it takes no click.
pub fn tick_box(state: TickState, enabled: bool, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let (fill, glyph): (Option<Hsla>, Option<Glyph>) = match state {
        TickState::Off => (None, None),
        TickState::Mixed => (Some(theme.primary), Some(Glyph::TickMixed)),
        TickState::On => (Some(theme.primary), Some(Glyph::TickOn)),
    };
    let mut tick = div()
        .flex_none()
        .size(style::TICK_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .rounded(style::HAIRLINE * 3.)
        .border(style::HAIRLINE)
        .border_color(fill.unwrap_or(theme.border));
    if let Some(fill) = fill {
        tick = tick.bg(fill);
    }
    if let Some(glyph) = glyph {
        tick = tick.child(
            Icon::new(icon(glyph))
                .size(style::TICK_SIZE - style::HAIRLINE * 4.)
                .text_color(theme.primary_foreground),
        );
    }
    if !enabled {
        tick = tick.opacity(DISABLED_OPACITY);
    }
    tick
}

/// How far a control that takes no click fades.
const DISABLED_OPACITY: f32 = 0.4;

/// A one-line muted note in place of a list.
fn placeholder(text: impl Into<gpui::SharedString>, cx: &App) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .px(style::MARGIN)
        .text_size(style::TEXT_CAPTION)
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// A section heading in the sidebar.
fn heading(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .id(text)
        .text_size(style::TEXT_CAPTION)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}
