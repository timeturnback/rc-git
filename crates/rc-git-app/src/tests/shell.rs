//! The shell's wiring against the fakes: opening, the ref list and picker, the lazy tree, the
//! hidden-files and wrap-lines toggles, modes, back and forward, and the tab restore.

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use gpui_component::tree::TreeEvent;
use rc_git_core::RefKind;
use rc_git_kit::back_forward::{ContentMode, Direction};
use rc_git_kit::browser::FileState;
use rc_git_kit::fake::{branch, oid};
use rc_git_kit::file_tree::ViewKind;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::{SettingsTabStore, WrapLinesPreference};
use rc_git_kit::tabs::OpenTabs;

use crate::app::{Shell, Tab};
use crate::list_rows::WrapLines;
use crate::menus::ShowHistory;
use crate::sidebar::refs::RefListEvent;
use crate::tests::support::{
    FEATURE_COMMIT, Harness, harness, harness_with, open_settled, repo, wait_until,
};
use crate::window::RepoWindow;

fn only_window(harness: &Harness, cx: &TestAppContext) -> Tab {
    let windows = harness
        .shell
        .read_with(cx, |shell, _| shell.windows().to_vec());
    assert_eq!(windows.len(), 1, "one window");
    windows[0].clone()
}

fn root_labels(window: &Entity<RepoWindow>, cx: &TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        let tree = window.read(cx).sidebar().read(cx).tree().clone();
        let model = window.read(cx).model().clone();
        let tree = tree.read(cx);
        let model = model.read(cx);
        tree.file_tree()
            .view(&|row| model.is_visible(row))
            .into_iter()
            .map(|node| node.label)
            .collect()
    })
}

fn stops(harness: &Harness, cx: &TestAppContext) -> usize {
    harness
        .shell
        .read_with(cx, |shell, _| shell.navigator().list().stops().len())
}

#[gpui::test]
fn opening_a_repository_shows_its_refs(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let labels = cx.read(|cx| window.read(cx).refs().read(cx).labels());
    assert_eq!(
        labels,
        ["# Branches", "feature", "main", "# Tags", "v1"],
        "a bare repository has no Working Tree row"
    );
    let banner = cx.read(|cx| window.read(cx).sidebar().read(cx).banner(cx));
    assert_eq!(banner.title, "main");
    assert_eq!(root_labels(&window, cx), ["src", "README.md"]);
}

#[gpui::test]
fn a_second_open_of_the_same_repository_reuses_its_tab(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    open_settled(&harness, cx, "/repo/a");
    harness
        .shell
        .update(cx, |shell, cx| shell.open("/repo/a/", cx));
    cx.run_until_parked();
    only_window(&harness, cx);
}

#[gpui::test]
fn a_path_that_does_not_open_opens_no_tab(cx: &mut TestAppContext) {
    let harness = harness(cx, &[]);
    let opened = harness
        .shell
        .update(cx, |shell, cx| shell.open("/nowhere", cx));
    assert_eq!(opened, None);
    let count = harness
        .shell
        .read_with(cx, |shell, _| shell.windows().len());
    assert_eq!(count, 0);
}

#[gpui::test]
fn picking_a_ref_in_the_picker_loads_its_tree(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_window(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.choose_ref(window_ref, cx));
    });
    let picker = cx.read(|cx| window.read(cx).picker().cloned());
    let picker = picker.unwrap_or_else(|| panic!("⌘B opens the picker in Files"));
    picker.update(cx, |_, cx| {
        cx.emit(RefListEvent::Picked(branch(
            "feature",
            oid(FEATURE_COMMIT),
            RefKind::LocalBranch,
        )));
    });
    wait_until(cx, |cx| root_labels(&window, cx) == ["feature.txt"]);
    assert!(
        cx.read(|cx| window.read(cx).picker().is_none()),
        "a pick closes it"
    );
}

#[gpui::test]
fn the_picker_stays_shut_in_changes(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_window(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| {
            view.set_mode(ContentMode::Changes, cx);
            view.choose_ref(window_ref, cx);
        });
    });
    assert!(cx.read(|cx| window.read(cx).picker().is_none()));
}

#[gpui::test]
fn expanding_a_folder_reads_one_tree_once(cx: &mut TestAppContext) {
    let repo = repo();
    let harness = harness(cx, &[("/repo/a", repo.clone())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tree = cx.read(|cx| window.read(cx).sidebar().read(cx).tree().clone());
    let state = cx.read(|cx| tree.read(cx).tree_state().clone());
    let before = repo.state().tree_entry_calls;
    state.update(cx, |_, cx| cx.emit(TreeEvent::Expanded("1".into())));
    let children = |cx: &TestAppContext| {
        cx.read(|cx| {
            let model = window.read(cx).model().read(cx);
            let view = tree.read(cx).file_tree().view(&|row| model.is_visible(row));
            match &view[0].kind {
                ViewKind::Folder { children, .. } => children
                    .iter()
                    .map(|child| child.label.clone())
                    .collect::<Vec<_>>(),
                ViewKind::Leaf(_) | ViewKind::Placeholder => Vec::new(),
            }
        })
    };
    wait_until(cx, |cx| children(cx) == ["main.rs"]);
    assert_eq!(repo.state().tree_entry_calls, before + 1);
    state.update(cx, |_, cx| {
        cx.emit(TreeEvent::Collapsed("1".into()));
        cx.emit(TreeEvent::Expanded("1".into()));
    });
    cx.run_until_parked();
    assert_eq!(
        repo.state().tree_entry_calls,
        before + 1,
        "a folder read once is not read again"
    );
}

#[gpui::test]
fn the_hidden_files_toggle_refilters_without_a_read(cx: &mut TestAppContext) {
    let repo = repo();
    let harness = harness(cx, &[("/repo/a", repo.clone())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let reads = repo.state().tree_entry_calls;
    harness.shell.update(cx, Shell::toggle_hidden_files);
    cx.run_until_parked();
    assert_eq!(root_labels(&window, cx), [".git", "src", "README.md"]);
    harness.shell.update(cx, Shell::toggle_hidden_files);
    cx.run_until_parked();
    assert_eq!(root_labels(&window, cx), ["src", "README.md"]);
    assert_eq!(repo.state().tree_entry_calls, reads);
}

#[gpui::test]
fn the_wrap_lines_toggle_starts_from_the_saved_choice_and_flips_every_diff(
    cx: &mut TestAppContext,
) {
    let settings = Arc::new(MemorySettings::new());
    WrapLinesPreference::new(settings.clone()).toggle().unwrap();
    let harness = harness_with(cx, &[], &settings);
    let wraps = |cx: &TestAppContext| {
        cx.read(|cx| {
            (
                WrapLines::on(cx),
                harness.shell.read(cx).menu_state(cx).wraps_lines,
            )
        })
    };
    assert_eq!(
        wraps(cx),
        (false, false),
        "the saved choice turned the default off, before any toggle"
    );
    harness.shell.update(cx, Shell::toggle_wrap_lines);
    assert_eq!(wraps(cx), (true, true));
    assert!(WrapLinesPreference::new(settings.clone()).wraps(), "saved");
    harness.shell.update(cx, Shell::toggle_wrap_lines);
    assert_eq!(wraps(cx), (false, false));
}

#[gpui::test]
fn diff_lines_wrap_until_the_user_turns_it_off(cx: &mut TestAppContext) {
    let harness = harness(cx, &[]);
    let wraps = cx.read(|cx| {
        (
            WrapLines::on(cx),
            harness.shell.read(cx).menu_state(cx).wraps_lines,
        )
    });
    assert_eq!(wraps, (true, true));
}

#[gpui::test]
fn selecting_a_file_row_opens_one_blob(cx: &mut TestAppContext) {
    let repo = repo();
    let harness = harness(cx, &[("/repo/a", repo.clone())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tree = cx.read(|cx| window.read(cx).sidebar().read(cx).tree().clone());
    let state = cx.read(|cx| tree.read(cx).tree_state().clone());
    let before = repo.state().read_blob_calls;
    state.update(cx, |state, cx| {
        let index = state.index_of(&"2".into());
        state.set_selected_index(index, cx);
    });
    wait_until(cx, |cx| {
        cx.read(|cx| {
            matches!(
                window.read(cx).model().read(cx).file_state(),
                FileState::Text(..)
            )
        })
    });
    assert_eq!(repo.state().read_blob_calls, before + 1);
    let preview = cx.read(|cx| window.read(cx).pane().read(cx).preview().cloned());
    assert!(preview.is_some(), "README.md shows as the markdown preview");
}

#[gpui::test]
fn a_mode_switch_from_the_menu_is_one_stop_and_back_returns(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_window(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, _| window_ref.activate_window());
    visual.run_until_parked();
    let start = stops(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(ShowHistory);
    visual.run_until_parked();
    assert_eq!(cx.read(|cx| window.read(cx).mode()), ContentMode::History);
    assert_eq!(stops(&harness, cx), start + 1, "one click, one stop");
    harness
        .shell
        .update(cx, |shell, cx| shell.go(Direction::Back, cx));
    wait_until(cx, |cx| {
        cx.read(|cx| window.read(cx).mode()) == ContentMode::Files
    });
    assert_eq!(
        stops(&harness, cx),
        start + 1,
        "a step back records nothing"
    );
}

#[gpui::test]
fn the_restore_reopens_saved_tabs_in_order_and_names_the_failures(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let saved = vec![
        "/repo/a".to_owned(),
        "/repo/b".to_owned(),
        "/tmp/rc-git-gone".to_owned(),
        "/repo/c".to_owned(),
    ];
    OpenTabs::new(SettingsTabStore::new(settings.clone())).save(&saved, 1);
    let harness = harness_with(
        cx,
        &[
            ("/repo/a", repo()),
            ("/repo/b", repo()),
            ("/repo/c", repo()),
        ],
        &settings,
    );
    harness.shell.update(cx, Shell::restore_tabs);
    cx.run_until_parked();
    let paths: Vec<String> = cx.read(|cx| {
        harness
            .shell
            .read(cx)
            .windows()
            .iter()
            .filter_map(|tab| tab.view.read(cx).shown().map(|shown| shown.path.clone()))
            .collect()
    });
    assert_eq!(paths, ["/repo/a", "/repo/b", "/repo/c"]);
    let tabs = OpenTabs::new(SettingsTabStore::new(settings));
    assert_eq!(
        tabs.paths(),
        ["/repo/a", "/repo/b", "/repo/c"],
        "a repository gone from a mounted folder is dropped"
    );
    assert_eq!(tabs.selected_index(), 1);
    let notices: Vec<bool> = cx.read(|cx| {
        harness
            .shell
            .read(cx)
            .windows()
            .iter()
            .map(|tab| tab.view.read(cx).notice().is_some())
            .collect()
    });
    assert_eq!(
        notices,
        [false, true, false],
        "the selected tab names the failure"
    );
}

#[gpui::test]
fn a_restore_with_nothing_saved_opens_one_empty_tab(cx: &mut TestAppContext) {
    let harness = harness(cx, &[]);
    harness.shell.update(cx, Shell::restore_tabs);
    cx.run_until_parked();
    let tab = only_window(&harness, cx);
    assert!(cx.read(|cx| tab.view.read(cx).shown().is_none()));
}

#[gpui::test]
fn a_narrow_window_collapses_the_sidebar_and_widening_reopens_it(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_window(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    let collapsed = |cx: &mut VisualTestContext| cx.read(|cx| window.read(cx).layout().collapsed());
    visual.simulate_resize(gpui::size(gpui::px(900.), gpui::px(600.)));
    visual.run_until_parked();
    assert!(!collapsed(&mut visual));
    visual.simulate_resize(gpui::size(gpui::px(559.), gpui::px(600.)));
    visual.run_until_parked();
    assert!(collapsed(&mut visual), "below the sidebar threshold");
    visual.simulate_resize(gpui::size(gpui::px(900.), gpui::px(600.)));
    visual.run_until_parked();
    assert!(
        !collapsed(&mut visual),
        "the policy re-opens what it closed"
    );
}

#[gpui::test]
fn a_moved_ref_reloads_through_the_watcher(cx: &mut TestAppContext) {
    let repo = repo();
    let harness = harness(cx, &[("/repo/a", repo.clone())]);
    open_settled(&harness, cx, "/repo/a");
    let before = repo.state().refs_calls;
    let fired = harness.watchers.emit(&["/repo/a/.git/refs/heads/main"]);
    assert_eq!(fired, 1);
    wait_until(cx, |_| repo.state().refs_calls == before + 1);
}

/// A shell showing a and b, with b's window closed; answers the shell, b's closed tab, and
/// the settings the shell saves its tabs to.
fn with_b_closed(cx: &mut TestAppContext) -> (Harness, Tab, Arc<MemorySettings>) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[("/repo/a", repo()), ("/repo/b", repo())], &settings);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let closing = harness
        .shell
        .read_with(cx, |shell, _| shell.windows()[1].clone());
    closing
        .handle
        .update(cx, |_, window, _| window.remove_window())
        .unwrap_or_else(|error| panic!("closing /repo/b: {error:#}"));
    cx.run_until_parked();
    (harness, closing, settings)
}

#[gpui::test]
fn a_closed_window_leaves_the_shell_and_its_repository_reopens(cx: &mut TestAppContext) {
    let (harness, closing, settings) = with_b_closed(cx);
    only_window(&harness, cx);
    let saved = OpenTabs::new(SettingsTabStore::new(settings)).paths();
    assert_eq!(
        saved,
        ["/repo/a"],
        "a tab closed on its own leaves the saved tabs"
    );

    let reopened = harness
        .shell
        .update(cx, |shell, cx| shell.open("/repo/b", cx));
    cx.run_until_parked();
    let reopened = reopened.expect("/repo/b reopens");
    assert_ne!(reopened, closing.id(), "a new window, not the closed one");
    let live = cx.update(|cx| {
        cx.windows()
            .iter()
            .any(|window| window.window_id() == reopened)
    });
    assert!(live, "the reopened window is open");
}

#[gpui::test]
fn reopen_closed_tab_brings_back_a_closed_window(cx: &mut TestAppContext) {
    let (harness, closing, _) = with_b_closed(cx);

    harness.shell.update(cx, Shell::reopen_closed_tab);
    cx.run_until_parked();
    let (paths, reopened) = harness.shell.read_with(cx, |shell, cx| {
        let paths: Vec<String> = shell
            .windows()
            .iter()
            .filter_map(|tab| tab.view.read(cx).shown().map(|shown| shown.path.clone()))
            .collect();
        (paths, shell.windows()[1].id())
    });
    assert_eq!(paths, ["/repo/a", "/repo/b"]);
    assert_ne!(reopened, closing.id(), "a new window, not the closed one");
}

#[gpui::test]
fn a_wrap_lines_toggle_that_cannot_save_still_checks_what_the_diffs_show(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[], &settings);
    settings.fail_writes(true);
    harness.shell.update(cx, Shell::toggle_wrap_lines);
    let wraps = cx.read(|cx| {
        (
            WrapLines::on(cx),
            harness.shell.read(cx).menu_state(cx).wraps_lines,
        )
    });
    assert_eq!(wraps, (false, false), "the checkmark follows the diffs");
}

#[gpui::test]
fn expand_diff_does_nothing_in_files(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    assert_eq!(cx.read(|cx| window.read(cx).mode()), ContentMode::Files);
    window.update(cx, RepoWindow::toggle_diff_expanded);
    assert!(!cx.read(|cx| window.read(cx).diff_expanded(cx)));
    window.update(cx, |window, cx| window.set_mode(ContentMode::History, cx));
    assert!(
        !cx.read(|cx| window.read(cx).diff_expanded(cx)),
        "History opens with its panes, as the user left them"
    );
}
