//! Every settings key rc-git reads or writes, in one table. Each string is the key the Swift
//! app used, so tabs, recents, approvals and preferences carry over; a test pins every one.
//! Keys a kit module already names are taken from it, so the module and the table cannot
//! drift.

/// The open tabs' paths, in tab order (`OpenTabs.swift`).
pub const OPEN_TABS: &str = crate::tabs::PATHS_KEY;
/// The selected tab's index (`OpenTabs.swift`).
pub const OPEN_TABS_SELECTED: &str = crate::tabs::SELECTED_KEY;
/// The recent repositories, newest first (`RecentRepositories.swift`).
pub const RECENT_REPOSITORIES: &str = crate::recent_repositories::KEY;
/// Whether hidden files are shown (`HiddenFiles.swift`).
pub const SHOW_HIDDEN_FILES: &str = crate::hidden_files::PREFERENCE_KEY;
/// The saved commit identity per repository (`CommitIdentity.swift`).
pub const COMMIT_IDENTITIES: &str = crate::commit_identity::SavedIdentities::KEY;
/// Approved ssh host-key fingerprints, host to `SHA256:...` (`HostKeyStore.swift`). The only
/// key spelled `rcgit.` rather than `rc-git.`; kept as the Swift app wrote it.
pub const ACCEPTED_HOST_KEYS: &str = "rcgit.acceptedHostKeys";
/// Whether the user dragged the sidebar divider (`LayoutPolicy.swift`, `SidebarWidthPin`).
pub const SIDEBAR_WIDTH_PINNED: &str = "rc-git.sidebar-width-pinned.v2";
/// The working tree's change-list grouping (`ChangeTree.swift`).
pub const CHANGES_GROUPING: &str = "rc-git.changesGrouping";
/// A History commit's changed-files grouping (`ChangeTree.swift`).
pub const COMMIT_CHANGES_GROUPING: &str = "rc-git.commitChangesGrouping";

/// The main window's frame autosave name (`MainWindowController.swift`).
pub const MAIN_WINDOW_FRAME: &str = "rc-git.window";
/// The update window's frame autosave name (`UpdateWindowController.swift`).
pub const UPDATE_WINDOW_FRAME: &str = "rc-git.update.v1";
/// The Settings window's frame autosave name (`SettingsWindowController.swift`).
pub const SETTINGS_WINDOW_FRAME: &str = "rc-git.settings.v1";
/// The sidebar | content split's autosave name (`MainWindowController.swift`).
pub const MAIN_SPLIT: &str = "rc-git.main.v2";
/// History's left column | diff split's autosave name (`HistoryViewController.swift`).
pub const HISTORY_SPLIT: &str = "rc-git.history.v5";
/// History's commit list over detail split's autosave name (`HistoryViewController.swift`).
pub const HISTORY_COLUMN_SPLIT: &str = "rc-git.history-column.v1";

/// The defaults key `AppKit` stores a window frame under for `name`.
#[must_use]
pub fn window_frame_key(name: &str) -> String {
    format!("NSWindow Frame {name}")
}

/// The defaults key `AppKit` stores a split view's subview frames under for `name`.
#[must_use]
pub fn split_view_key(name: &str) -> String {
    format!("NSSplitView Subview Frames {name}")
}

/// Every plain key, for a test or a migration that walks them all.
pub const ALL: [&str; 9] = [
    OPEN_TABS,
    OPEN_TABS_SELECTED,
    RECENT_REPOSITORIES,
    SHOW_HIDDEN_FILES,
    COMMIT_IDENTITIES,
    ACCEPTED_HOST_KEYS,
    SIDEBAR_WIDTH_PINNED,
    CHANGES_GROUPING,
    COMMIT_CHANGES_GROUPING,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_the_string_the_swift_app_wrote() {
        assert_eq!(
            ALL,
            [
                "rc-git.openTabs",
                "rc-git.openTabs.selected",
                "rc-git.recentRepositories",
                "rc-git.showHiddenFiles",
                "rc-git.commitIdentities",
                "rcgit.acceptedHostKeys",
                "rc-git.sidebar-width-pinned.v2",
                "rc-git.changesGrouping",
                "rc-git.commitChangesGrouping",
            ]
        );
    }

    #[test]
    fn appkit_autosave_keys_are_the_ones_appkit_wrote() {
        assert_eq!(
            window_frame_key(MAIN_WINDOW_FRAME),
            "NSWindow Frame rc-git.window"
        );
        assert_eq!(
            window_frame_key(UPDATE_WINDOW_FRAME),
            "NSWindow Frame rc-git.update.v1"
        );
        assert_eq!(
            window_frame_key(SETTINGS_WINDOW_FRAME),
            "NSWindow Frame rc-git.settings.v1"
        );
        assert_eq!(
            split_view_key(MAIN_SPLIT),
            "NSSplitView Subview Frames rc-git.main.v2"
        );
        assert_eq!(
            split_view_key(HISTORY_SPLIT),
            "NSSplitView Subview Frames rc-git.history.v5"
        );
        assert_eq!(
            split_view_key(HISTORY_COLUMN_SPLIT),
            "NSSplitView Subview Frames rc-git.history-column.v1"
        );
    }
}
