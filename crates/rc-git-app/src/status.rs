//! The one place a change kind becomes a color (`StatusPalette.swift`): the History file list
//! reads it now, and the Changes list will. Resolved from the active theme at render, so a
//! light/dark switch recolors on the next frame.

use gpui::{App, Hsla};
use gpui_component::ActiveTheme;
use rc_git_core::{ChangeKind, WorktreeStatus};

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

/// The color of a working-tree status letter in the Changes list: modified orange (the
/// theme's yellow), untracked green, deleted red, ignored muted, unchanged plain.
#[must_use]
pub fn worktree_color(status: WorktreeStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        WorktreeStatus::Unchanged => theme.foreground,
        WorktreeStatus::Modified => theme.yellow,
        WorktreeStatus::Untracked => theme.green,
        WorktreeStatus::Ignored => theme.muted_foreground,
        WorktreeStatus::Deleted => theme.red,
    }
}
