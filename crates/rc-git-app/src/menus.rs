//! The main menu (`MainMenu.swift`) and every key binding, built from kit's
//! [`SHORTCUTS`] table: one action per command, and one binding per table row. A command
//! whose pane lands in a later phase has its action and binding but no handler, so GPUI
//! shows it disabled until the phase that wires it.

use gpui::{Action, App, KeyBinding, Menu, MenuItem, OsAction, SharedString, actions};
use gpui_component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::menu_shortcuts::{SHORTCUTS, search_title};
use rc_git_kit::recent_repositories::display_path;
use rc_git_kit::sync::back_to_default_title;

actions!(
    rc_git,
    [
        /// Settings (Settings window, phase 6).
        Settings,
        /// Hides the app.
        HideApp,
        /// Hides every other app.
        HideOthers,
        /// Shows every app.
        ShowAll,
        /// Quits.
        Quit,
        /// Rebuilds and relaunches the bundle (updater, phase 7).
        RebuildNow,
        /// Opens the update window (updater, phase 7).
        ShowUpdate,
        /// The About window: the bundle's name and the build it was stamped with.
        ShowAbout,
        /// Adds an empty tab.
        NewTab,
        /// Reopens the most recently closed tab's repository.
        ReopenClosedTab,
        /// Chooses a repository with the Open panel.
        OpenRepository,
        /// The recent-repository picker (phase 6).
        OpenRecentRepository,
        /// Empties Open Recent.
        ClearRecents,
        /// Go to File (phase 6).
        GoToFile,
        /// Opens the ref picker under the banner.
        GoToBranch,
        /// Closes the tab on screen and drops it from the next launch.
        CloseTab,
        /// Closes every tab of the front window and keeps them for the next launch.
        CloseWindow,
        /// Paste and Match Style; no field takes styled text.
        PasteAndMatchStyle,
        /// Deletes the selection in a field.
        DeleteSelection,
        /// Shows the find bar.
        Find,
        /// The next find match.
        FindNext,
        /// The previous find match.
        FindPrevious,
        /// Puts the selection in the find field.
        UseSelectionForFind,
        /// Files mode.
        ShowFiles,
        /// History mode.
        ShowHistory,
        /// Changes mode.
        ShowChanges,
        /// Steps back through the app-wide list.
        GoBack,
        /// Steps forward through the app-wide list.
        GoForward,
        /// Shows or hides hidden files in every tab.
        ToggleHiddenFiles,
        /// Wraps long diff and file lines, or lets them scroll sideways, in every tab.
        ToggleWrapLines,
        /// Shows the diff across the whole window, or puts the panes beside it back.
        ToggleDiffExpanded,
        /// Blame (History, phase 4).
        ShowBlame,
        /// History of this file (phase 4).
        ShowFileHistory,
        /// Find in Repository or Search Commits (phases 4 and 6).
        SearchCommits,
        /// Shows or hides the sidebar.
        ToggleSidebar,
        /// Enters or leaves full screen.
        ToggleFullScreen,
        /// Fetch (phase 5).
        Fetch,
        /// Pull (phase 5).
        Pull,
        /// Push (phase 5).
        Push,
        /// Sync (phase 5).
        Sync,
        /// Back to the default branch (phase 5).
        BackToDefault,
        /// Cancels a running sync (phase 5).
        CancelSync,
        /// The commit identity sheet (phase 5).
        CommitIdentity,
        /// Minimizes the window.
        Minimize,
        /// Zooms the window.
        Zoom,
        /// Selects the previous tab.
        PreviousTab,
        /// Selects the next tab.
        NextTab,
        /// Moves the tab into a window of its own.
        MoveTabToNewWindow,
        /// Merges every window into one tab group.
        MergeAllWindows,
        /// Brings every window to the front.
        BringAllToFront
    ]
);

/// Opens one repository from Open Recent.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rc_git, no_json)]
pub struct OpenRecent {
    /// The repository's path.
    pub path: String,
}

/// The action of the table row `id`, `None` for an id no action answers.
#[must_use]
pub fn action_for(id: &str) -> Option<Box<dyn Action>> {
    let action: Box<dyn Action> = match id {
        "settings" => Box::new(Settings),
        "hide_app" => Box::new(HideApp),
        "hide_others" => Box::new(HideOthers),
        "quit" => Box::new(Quit),
        "rebuild_now" => Box::new(RebuildNow),
        "new_tab" => Box::new(NewTab),
        "reopen_closed_tab" => Box::new(ReopenClosedTab),
        "open_repository" => Box::new(OpenRepository),
        "open_recent_repository" => Box::new(OpenRecentRepository),
        "go_to_file" => Box::new(GoToFile),
        "go_to_branch" => Box::new(GoToBranch),
        "close_tab" => Box::new(CloseTab),
        "close_window" => Box::new(CloseWindow),
        "undo" => Box::new(Undo),
        "redo" => Box::new(Redo),
        "cut" => Box::new(Cut),
        "copy" => Box::new(Copy),
        "paste" => Box::new(Paste),
        "paste_and_match_style" => Box::new(PasteAndMatchStyle),
        "select_all" => Box::new(SelectAll),
        _ => return view_action(id),
    };
    Some(action)
}

fn view_action(id: &str) -> Option<Box<dyn Action>> {
    let action: Box<dyn Action> = match id {
        "find" => Box::new(Find),
        "find_next" => Box::new(FindNext),
        "find_previous" => Box::new(FindPrevious),
        "use_selection_for_find" => Box::new(UseSelectionForFind),
        "files" => Box::new(ShowFiles),
        "history" => Box::new(ShowHistory),
        "changes" => Box::new(ShowChanges),
        "back" => Box::new(GoBack),
        "forward" => Box::new(GoForward),
        "show_hidden_files" => Box::new(ToggleHiddenFiles),
        "show_blame" => Box::new(ShowBlame),
        "file_history" => Box::new(ShowFileHistory),
        "search_commits" => Box::new(SearchCommits),
        "toggle_sidebar" => Box::new(ToggleSidebar),
        "enter_full_screen" => Box::new(ToggleFullScreen),
        _ => return window_action(id),
    };
    Some(action)
}

fn window_action(id: &str) -> Option<Box<dyn Action>> {
    let action: Box<dyn Action> = match id {
        "fetch" => Box::new(Fetch),
        "pull" => Box::new(Pull),
        "push" => Box::new(Push),
        "sync" => Box::new(Sync),
        "back_to_default" => Box::new(BackToDefault),
        "cancel_sync" => Box::new(CancelSync),
        "minimize" => Box::new(Minimize),
        "previous_tab" => Box::new(PreviousTab),
        "next_tab" => Box::new(NextTab),
        _ => return None,
    };
    Some(action)
}

/// One binding per table row, in table order.
///
/// # Errors
/// When a row's keystroke does not parse; names the row.
pub fn key_bindings(cx: &App) -> anyhow::Result<Vec<KeyBinding>> {
    let mut bindings = Vec::with_capacity(SHORTCUTS.len());
    for row in SHORTCUTS {
        let action = action_for(row.id)
            .ok_or_else(|| anyhow::anyhow!("menu shortcut '{}' has no action", row.id))?;
        let binding = KeyBinding::load(row.keystroke, action, None, false, None, cx)
            .map_err(|error| anyhow::anyhow!("bind '{}' ({}): {error:?}", row.id, row.keystroke))?;
        bindings.push(binding);
    }
    Ok(bindings)
}

/// What the menus show that changes while the app runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    /// The app's name, in About, Hide and Quit.
    pub app_name: String,
    /// Open Recent's paths, newest first.
    pub recents: Vec<String>,
    /// The home directory, which Open Recent abbreviates to `~`.
    pub home: String,
    /// Whether hidden files show, for the checkmark.
    pub shows_hidden_files: bool,
    /// Whether long lines wrap, for the checkmark.
    pub wraps_lines: bool,
    /// Whether the front tab shows its diff across the whole window, for the checkmark.
    pub diff_expanded: bool,
    /// The front tab's mode, which titles the search command.
    pub mode: ContentMode,
    /// The front tab's default branch, which titles Back to Default.
    pub default_branch: Option<String>,
    /// The front tab's Pull and Push titles, with the counts of its last status.
    pub sync_titles: Option<(String, String)>,
}

fn title_of(id: &str) -> &'static str {
    SHORTCUTS
        .iter()
        .find(|row| row.id == id)
        .map_or("", |row| row.title)
}

fn item(id: &str) -> Option<MenuItem> {
    let action = action_for(id)?;
    Some(MenuItem::Action {
        name: title_of(id).into(),
        action,
        os_action: None,
        checked: false,
        disabled: false,
    })
}

/// View > Wrap Lines, which has no shortcut row.
pub const WRAP_LINES_TITLE: &str = "Wrap Lines";
/// View > Expand Diff, which has no shortcut row.
pub const EXPAND_DIFF_TITLE: &str = "Expand Diff";

fn titled(title: impl Into<SharedString>, action: impl Action) -> MenuItem {
    MenuItem::action(title, action)
}

/// The whole menu bar for `state`.
#[must_use]
pub fn menus(state: &MenuState) -> Vec<Menu> {
    vec![
        app_menu(state),
        file_menu(state),
        edit_menu(),
        view_menu(state),
        repository_menu(state),
        window_menu(),
    ]
}

fn app_menu(state: &MenuState) -> Menu {
    let name = &state.app_name;
    // `MainMenu.appMenu`: the bundle rebuilds the working tree and earns ⌥⌘R, its edit-run
    // loop. A build with no checkout shows the pair disabled, since it has no handler.
    Menu::new(name.clone()).items(
        [
            Some(titled(format!("About {name}"), ShowAbout)),
            Some(MenuItem::separator()),
            item("settings"),
            Some(MenuItem::separator()),
            Some(titled("Rebuild and Relaunch\u{2026}", ShowUpdate)),
            item("rebuild_now"),
            Some(MenuItem::separator()),
            Some(titled(format!("Hide {name}"), HideApp)),
            item("hide_others"),
            Some(titled("Show All", ShowAll)),
            Some(MenuItem::separator()),
            Some(titled(format!("Quit {name}"), Quit)),
        ]
        .into_iter()
        .flatten(),
    )
}

fn file_menu(state: &MenuState) -> Menu {
    let mut recent: Vec<MenuItem> = state
        .recents
        .iter()
        .map(|path| {
            titled(
                display_path(path, &state.home),
                OpenRecent { path: path.clone() },
            )
        })
        .collect();
    if !recent.is_empty() {
        recent.push(MenuItem::separator());
    }
    recent.push(titled("Clear Menu", ClearRecents));
    Menu::new("File").items(
        [
            item("new_tab"),
            item("reopen_closed_tab"),
            item("open_repository"),
            Some(MenuItem::submenu(Menu::new("Open Recent").items(recent))),
            item("open_recent_repository"),
            Some(MenuItem::separator()),
            item("go_to_file"),
            item("go_to_branch"),
            Some(MenuItem::separator()),
            item("close_tab"),
            item("close_window"),
        ]
        .into_iter()
        .flatten(),
    )
}

fn os(id: &str, os_action: OsAction) -> Option<MenuItem> {
    let action = action_for(id)?;
    Some(MenuItem::Action {
        name: title_of(id).into(),
        action,
        os_action: Some(os_action),
        checked: false,
        disabled: false,
    })
}

fn edit_menu() -> Menu {
    let find = Menu::new("Find").items(
        [
            item("find"),
            item("find_next"),
            item("find_previous"),
            item("use_selection_for_find"),
        ]
        .into_iter()
        .flatten(),
    );
    Menu::new("Edit").items(
        [
            os("undo", OsAction::Undo),
            os("redo", OsAction::Redo),
            Some(MenuItem::separator()),
            os("cut", OsAction::Cut),
            os("copy", OsAction::Copy),
            os("paste", OsAction::Paste),
            item("paste_and_match_style"),
            Some(titled("Delete", DeleteSelection)),
            os("select_all", OsAction::SelectAll),
            Some(MenuItem::separator()),
            Some(MenuItem::submenu(find)),
        ]
        .into_iter()
        .flatten(),
    )
}

fn view_menu(state: &MenuState) -> Menu {
    let hidden = item("show_hidden_files").map(|item| item.checked(state.shows_hidden_files));
    let wrap = titled(WRAP_LINES_TITLE, ToggleWrapLines).checked(state.wraps_lines);
    let expand = titled(EXPAND_DIFF_TITLE, ToggleDiffExpanded).checked(state.diff_expanded);
    Menu::new("View").items(
        [
            item("files"),
            item("history"),
            item("changes"),
            Some(MenuItem::separator()),
            item("back"),
            item("forward"),
            Some(MenuItem::separator()),
            hidden,
            Some(wrap),
            Some(expand),
            Some(MenuItem::separator()),
            item("show_blame"),
            item("file_history"),
            Some(titled(search_title(state.mode), SearchCommits)),
            Some(MenuItem::separator()),
            item("toggle_sidebar"),
            item("enter_full_screen"),
        ]
        .into_iter()
        .flatten(),
    )
}

fn repository_menu(state: &MenuState) -> Menu {
    Menu::new("Repository").items(
        [
            item("fetch"),
            Some(titled(
                state
                    .sync_titles
                    .as_ref()
                    .map_or_else(|| title_of("pull").to_owned(), |titles| titles.0.clone()),
                Pull,
            )),
            Some(titled(
                state
                    .sync_titles
                    .as_ref()
                    .map_or_else(|| title_of("push").to_owned(), |titles| titles.1.clone()),
                Push,
            )),
            Some(MenuItem::separator()),
            item("sync"),
            Some(titled(
                back_to_default_title(state.default_branch.as_deref()),
                BackToDefault,
            )),
            item("cancel_sync"),
            Some(MenuItem::separator()),
            Some(titled("Commit Identity\u{2026}", CommitIdentity)),
        ]
        .into_iter()
        .flatten(),
    )
}

fn window_menu() -> Menu {
    Menu::new("Window").items(
        [
            item("minimize"),
            Some(titled("Zoom", Zoom)),
            Some(MenuItem::separator()),
            item("previous_tab"),
            item("next_tab"),
            Some(titled("Move Tab to New Window", MoveTabToNewWindow)),
            Some(titled("Merge All Windows", MergeAllWindows)),
            Some(MenuItem::separator()),
            Some(titled("Bring All to Front", BringAllToFront)),
        ]
        .into_iter()
        .flatten(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(menu: &Menu, out: &mut Vec<String>) {
        for item in &menu.items {
            match item {
                MenuItem::Action { action, .. } => out.push(action.name().to_owned()),
                MenuItem::Submenu(submenu) => names(submenu, out),
                MenuItem::Separator | MenuItem::SystemMenu(_) => {}
            }
        }
    }

    #[test]
    fn every_table_row_has_an_action() {
        for row in SHORTCUTS {
            assert!(action_for(row.id).is_some(), "{} has no action", row.id);
        }
    }

    #[test]
    fn no_two_rows_share_an_action() {
        let mut seen = std::collections::HashSet::new();
        for row in SHORTCUTS {
            let name = action_for(row.id).map(|action| action.name().to_owned());
            assert!(seen.insert(name), "{} reuses an action", row.id);
        }
    }

    #[gpui::test]
    fn every_table_row_is_bound(cx: &mut gpui::TestAppContext) {
        let bindings = cx.update(|cx| key_bindings(cx));
        let bindings = bindings.unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(bindings.len(), SHORTCUTS.len());
        for (binding, row) in bindings.iter().zip(SHORTCUTS) {
            let expected = action_for(row.id).map(|action| action.name().to_owned());
            assert_eq!(
                Some(binding.action().name().to_owned()),
                expected,
                "{}",
                row.id
            );
        }
    }

    #[test]
    fn every_bound_command_is_in_a_menu() {
        let state = MenuState {
            app_name: "rc-git".to_owned(),
            recents: vec!["/tmp/a".to_owned()],
            home: "/Users/me".to_owned(),
            shows_hidden_files: false,
            wraps_lines: false,
            diff_expanded: false,
            mode: ContentMode::Files,
            default_branch: None,
            sync_titles: None,
        };
        let mut shown = Vec::new();
        for menu in menus(&state) {
            names(&menu, &mut shown);
        }
        for row in SHORTCUTS {
            let Some(action) = action_for(row.id) else {
                continue;
            };
            let name = action.name();
            assert!(
                shown.iter().any(|shown| shown == name),
                "{} ({name}) is in no menu",
                row.id
            );
        }
    }

    fn app_items() -> Vec<String> {
        let state = MenuState {
            app_name: "rc-git".to_owned(),
            recents: Vec::new(),
            home: "/Users/me".to_owned(),
            shows_hidden_files: false,
            wraps_lines: false,
            diff_expanded: false,
            mode: ContentMode::Files,
            default_branch: None,
            sync_titles: None,
        };
        let mut titles = Vec::new();
        for item in app_menu(&state).items {
            if let MenuItem::Action { name, .. } = item {
                titles.push(name.to_string());
            }
        }
        titles
    }

    #[test]
    fn the_app_menu_offers_the_rebuild_pair() {
        let items = app_items();
        assert!(
            items.contains(&"Rebuild and Relaunch\u{2026}".to_owned()),
            "{items:?}"
        );
        assert!(
            items.contains(&title_of("rebuild_now").to_owned()),
            "{items:?}"
        );
        assert!(!items.iter().any(|title| title.starts_with("Update ")));
        assert_eq!(items.first().map(String::as_str), Some("About rc-git"));
    }
}
