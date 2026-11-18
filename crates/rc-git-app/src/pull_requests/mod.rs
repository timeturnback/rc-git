//! The pull request views: the sidebar's section ([`section`]) and the pull request
//! page with its tabs ([`page`]) and their Changes view ([`changes`]). All draw kit's
//! [`PullRequestsModel`] and send every click
//! back to it; none reads the network or the repository, or decides anything.
//!
//! [`PullRequestsModel`]: rc_git_kit::pull_requests::model::PullRequestsModel

pub mod changes;
pub mod page;
pub mod section;

use gpui::IntoElement;
use gpui::Styled;
use gpui_component::{Icon, Theme};
use rc_git_kit::pull_requests::checks::CheckState;

use crate::icons::{Glyph, icon};
use crate::style;

/// The icon of a check state, tinted by it.
pub(crate) fn check_icon(state: CheckState, theme: &Theme) -> impl IntoElement + use<> {
    let (glyph, color) = match state {
        CheckState::Failing => (Glyph::CheckFailing, theme.red),
        CheckState::Pending => (Glyph::CheckPending, theme.warning),
        CheckState::Passing => (Glyph::CheckPassing, theme.green),
        CheckState::Neutral => (Glyph::CheckNeutral, theme.muted_foreground),
    };
    Icon::new(icon(glyph))
        .size(style::ICON_SIZE)
        .text_color(color)
}

/// Now, in seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    rc_git_kit::github::unix_now()
}
