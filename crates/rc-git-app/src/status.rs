//! The one place a change kind becomes a color (`StatusPalette.swift`): the History file list
//! reads it now, and the Changes list will. Resolved from the active theme at render, so a
//! light/dark switch recolors on the next frame.

use gpui::{App, Hsla};
use gpui_component::ActiveTheme;
use rc_git_core::ChangeKind;

/// The color of a change kind's letter: added green, deleted red, modified orange (the
/// theme's yellow), renamed blue.
#[must_use]
pub fn change_color(kind: ChangeKind, cx: &App) -> Hsla {
    let theme = cx.theme();
    match kind {
        ChangeKind::Added => theme.green,
        ChangeKind::Deleted => theme.red,
        ChangeKind::Modified => theme.yellow,
        ChangeKind::Renamed => theme.blue,
    }
}
