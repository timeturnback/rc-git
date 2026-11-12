//! The quick-pick panels and Settings against the fakes: the keys the field hands over, what a
//! pick opens, and what Settings removes.

use std::sync::Arc;

use gpui::{AppContext, Entity, TestAppContext, VisualTestContext};
use rc_git_core::{FileSearch, LineMatch};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::browser::FileState;
use rc_git_kit::content_search::ContentSearchStatus;
use rc_git_kit::fake::{FakeRepository, oid};
use rc_git_kit::platform::host_keys::{HostKeyStore, MemoryHostKeyStore};
use rc_git_kit::text_document::FileReveal;

use crate::app::Tab;
use crate::settings::SettingsView;
use crate::tests::support::{
    GIT_DIR_TREE, Harness, README, harness, open_settled, repo, wait_until,
};
use crate::window::RepoWindow;
use crate::window::panels::Showing;

/// The standard fake with its hidden `.git` folder readable, so the path index can walk it.
fn indexable_repo() -> FakeRepository {
    let repo = repo();
    repo.state().trees.insert(oid(GIT_DIR_TREE), Vec::new());
    repo
}

fn front(harness: &Harness, cx: &TestAppContext) -> Tab {
    harness
        .shell
        .read_with(cx, |shell, _| shell.windows()[0].clone())
}

fn showing(window: &Entity<RepoWindow>, cx: &TestAppContext) -> Option<Showing> {
    cx.read(|cx| window.read(cx).panels().showing())
}

fn opened(window: &Entity<RepoWindow>, cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        let model = window.read(cx).model().read(cx);
        matches!(model.file_state(), FileState::Text(..))
            .then(|| model.opened_file_path().map(str::to_owned))
            .flatten()
    })
}

#[gpui::test]
fn go_to_file_moves_with_the_arrows_and_opens_with_return(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", indexable_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.switch_mode(ContentMode::History, cx));
        window.update(cx, |view, cx| view.show_go_to_file(window_ref, cx));
    });
    assert_eq!(showing(&window, cx), Some(Showing::GoToFile));
    let panel = cx.read(|cx| window.read(cx).panels().go_to_file().cloned());
    let panel = panel.unwrap_or_else(|| panic!("the panel exists once shown"));
    wait_until(cx, |cx| cx.read(|cx| panel.read(cx).results(cx).len() == 2));
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_keystrokes("down");
    assert_eq!(cx.read(|cx| panel.read(cx).highlight()), Some(1));
    let picked = cx.read(|cx| panel.read(cx).results(cx)[1].path.clone());
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| {
        opened(&window, cx).as_deref() == Some(picked.as_str())
    });
    assert_eq!(showing(&window, cx), None, "a pick closes the panel");
    assert_eq!(cx.read(|cx| window.read(cx).mode()), ContentMode::Files);
}

#[gpui::test]
fn a_return_typed_ahead_of_the_results_opens_the_first_match(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", indexable_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.show_go_to_file(window_ref, cx));
    });
    visual.simulate_input("main");
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| {
        opened(&window, cx).as_deref() == Some("src/main.rs")
    });
}

#[gpui::test]
fn escape_closes_a_panel_and_go_to_file_needs_a_repository(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", indexable_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.show_go_to_file(window_ref, cx));
    });
    visual.simulate_keystrokes("escape");
    assert_eq!(showing(&window, cx), None);
    let empty = harness.shell.update(cx, crate::app::Shell::new_window);
    let empty = empty.unwrap_or_else(|| panic!("an empty tab opens"));
    let mut visual = VisualTestContext::from_window(empty.handle.into(), cx);
    visual.update(|window_ref, cx| {
        empty
            .view
            .update(cx, |view, cx| view.show_go_to_file(window_ref, cx));
    });
    assert_eq!(showing(&empty.view, cx), None, "an empty tab has no files");
}

/// The standard fake where every copy of the README blob matches `body` on its third line.
fn searchable_repo() -> FakeRepository {
    let repo = indexable_repo();
    repo.state().blob_searches.insert(
        oid(README),
        FileSearch::Searched {
            lines: vec![LineMatch {
                line: 2,
                column: 0,
                excerpt: b"body".to_vec(),
                ranges: std::iter::once(0..4).collect(),
            }],
            truncated: false,
        },
    );
    repo
}

#[gpui::test]
fn find_in_repository_searches_on_return_and_opens_the_match_as_source(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", searchable_repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.show_find_in_repository(window_ref, cx));
    });
    assert_eq!(showing(&window, cx), Some(Showing::Find));
    let panel = cx.read(|cx| window.read(cx).panels().find().cloned());
    let panel = panel.unwrap_or_else(|| panic!("the panel exists once shown"));
    let session = cx.read(|cx| panel.read(cx).session().clone());
    visual.simulate_input("body");
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| {
        cx.read(|cx| matches!(session.read(cx).status(), ContentSearchStatus::Finished(..)))
    });
    assert_eq!(
        cx.read(|cx| session.read(cx).rows().len()),
        4,
        "two files, a line each"
    );
    assert_eq!(
        cx.read(|cx| panel.read(cx).highlight()),
        Some(1),
        "the first match is highlighted"
    );
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| opened(&window, cx).is_some());
    let pane = cx.read(|cx| window.read(cx).pane().clone());
    assert!(
        cx.read(|cx| pane.read(cx).viewer().is_some()),
        "a match shows the source, even for markdown"
    );
    let pending: Option<FileReveal> =
        cx.read(|cx| window.read(cx).model().read(cx).pending_reveal().cloned());
    assert_eq!(pending, None, "the pane took the match");
    let rows = cx.read(|cx| session.read(cx).rows().len());
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.show_find_in_repository(window_ref, cx));
    });
    assert_eq!(
        cx.read(|cx| session.read(cx).rows().len()),
        rows,
        "the same root keeps its results"
    );
}

#[gpui::test]
fn the_recent_picker_filters_and_opens_the_pick(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/alpha", repo()), ("/repo/beta", repo())]);
    open_settled(&harness, cx, "/repo/beta");
    let window = open_settled(&harness, cx, "/repo/alpha");
    harness
        .shell
        .update(cx, crate::app::Shell::show_recent_picker);
    let panel = cx.read(|cx| window.read(cx).panels().recent().cloned());
    let panel = panel.unwrap_or_else(|| panic!("⌃R opens over the front window"));
    assert_eq!(
        cx.read(|cx| panel.read(cx).results().to_vec()),
        ["/repo/alpha", "/repo/beta"]
    );
    let tab = harness.shell.read_with(cx, |shell, cx| {
        shell
            .windows()
            .iter()
            .find(|tab| tab.view == window)
            .cloned()
            .unwrap_or_else(|| panic!("{}", cx.windows().len()))
    });
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_input("bet");
    assert_eq!(
        cx.read(|cx| panel.read(cx).results().to_vec()),
        ["/repo/beta"]
    );
    assert_eq!(
        cx.read(|cx| panel.read(cx).highlight()),
        Some(0),
        "the best match is the pick"
    );
    visual.simulate_keystrokes("enter");
    assert_eq!(showing(&window, cx), None);
    let count = harness
        .shell
        .read_with(cx, |shell, _| shell.windows().len());
    assert_eq!(
        count, 2,
        "an open repository's tab is brought forward, not reopened"
    );
}

#[gpui::test]
fn settings_lists_approved_hosts_and_removes_the_selection(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let store = Arc::new(MemoryHostKeyStore::new());
    for host in ["github.com", "a.example", "gitlab.com"] {
        let stored = store.store("SHA256:x", host);
        assert!(stored.is_ok());
    }
    let shared: Arc<dyn HostKeyStore> = store.clone();
    let view = cx.new(|_| SettingsView::new(shared));
    assert_eq!(
        cx.read(|cx| view.read(cx).hosts()),
        ["a.example", "github.com", "gitlab.com"]
    );
    let window = cx.add_window(|_, _| gpui::Empty);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window_ref, cx| {
        view.update(cx, |view, cx| {
            view.select(0, false, cx);
            view.select(2, true, cx);
            view.remove_selected(window_ref, cx);
        });
    });
    assert_eq!(cx.read(|cx| view.read(cx).hosts()), ["github.com"]);
    assert_eq!(store.approvals().len(), 1, "the store forgot them");
    let approved = store.store("SHA256:y", "new.example");
    assert!(approved.is_ok());
    view.update(cx, SettingsView::reload);
    assert_eq!(
        cx.read(|cx| view.read(cx).hosts()),
        ["github.com", "new.example"],
        "a host approved since shows on reload"
    );
}

#[gpui::test]
fn settings_opens_one_window_and_reuses_it(cx: &mut TestAppContext) {
    let harness = harness(cx, &[]);
    harness.shell.update(cx, crate::app::Shell::show_settings);
    let first = harness.shell.read_with(cx, |shell, _| {
        shell.settings_window().map(|(handle, _)| *handle)
    });
    assert!(first.is_some());
    harness.shell.update(cx, crate::app::Shell::show_settings);
    let second = harness.shell.read_with(cx, |shell, _| {
        shell.settings_window().map(|(handle, _)| *handle)
    });
    assert_eq!(first, second);
    assert_eq!(
        harness
            .shell
            .read_with(cx, |shell, _| shell.windows().len()),
        0,
        "Settings is not a repository tab"
    );
}

fn selected_ref(window: &Entity<RepoWindow>, cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        let model = window.read(cx).model().read(cx);
        model.selected_ref().map(|entry| entry.short_name.clone())
    })
}

/// Opens the ⌘B picker on `/repo/a` (rows: `# Branches`, feature, main, `# Tags`, v1).
fn open_picker(
    cx: &mut TestAppContext,
) -> (
    Entity<RepoWindow>,
    Entity<crate::sidebar::refs::RefListView>,
    Tab,
) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| view.choose_ref(window_ref, cx));
    });
    let picker = cx.read(|cx| window.read(cx).picker().cloned());
    let picker = picker.unwrap_or_else(|| panic!("⌘B opens the picker in Files"));
    (window, picker, tab)
}

#[gpui::test]
fn the_picker_arrows_step_over_headers_and_return_picks(cx: &mut TestAppContext) {
    let (window, picker, tab) = open_picker(cx);
    assert_eq!(
        cx.read(|cx| picker.read(cx).highlight()),
        Some(2),
        "the browsed ref starts highlighted"
    );
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_keystrokes("down");
    assert_eq!(
        cx.read(|cx| picker.read(cx).highlight()),
        Some(4),
        "over # Tags"
    );
    visual.simulate_keystrokes("down");
    assert_eq!(
        cx.read(|cx| picker.read(cx).highlight()),
        Some(4),
        "the last row holds"
    );
    assert_eq!(
        selected_ref(&window, cx).as_deref(),
        Some("main"),
        "arrows only highlight"
    );
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| selected_ref(&window, cx).as_deref() == Some("v1"));
    assert!(cx.read(|cx| window.read(cx).picker().is_none()));
}

#[gpui::test]
fn a_filter_highlights_its_best_match_for_return(cx: &mut TestAppContext) {
    let (window, picker, tab) = open_picker(cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_input("feat");
    assert_eq!(cx.read(|cx| picker.read(cx).highlight()), Some(1));
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| {
        selected_ref(&window, cx).as_deref() == Some("feature")
    });
}

#[gpui::test]
fn return_on_the_browsed_ref_closes_the_picker_with_nothing_to_load(cx: &mut TestAppContext) {
    let (window, _picker, tab) = open_picker(cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_keystrokes("enter");
    assert!(cx.read(|cx| window.read(cx).picker().is_none()));
    assert_eq!(selected_ref(&window, cx).as_deref(), Some("main"));
}

#[gpui::test]
fn the_sidebar_arrows_browse_as_they_move(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.update(|window_ref, cx| {
        window.update(cx, |view, cx| {
            view.switch_mode(ContentMode::History, cx);
            view.choose_ref(window_ref, cx);
        });
    });
    visual.simulate_keystrokes("up");
    wait_until(cx, |cx| {
        selected_ref(&window, cx).as_deref() == Some("feature")
    });
}

fn tree_selection(window: &Entity<RepoWindow>, cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| {
        let tree = window.read(cx).sidebar().read(cx).tree().clone();
        let state = tree.read(cx).tree_state().clone();
        state
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string())
    })
}

#[gpui::test]
fn the_tree_selection_follows_a_file_opened_elsewhere(cx: &mut TestAppContext) {
    let fake = repo();
    let harness = harness(cx, &[("/repo/a", fake.clone())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let model = cx.read(|cx| window.read(cx).model().clone());
    let reads = fake.state().read_blob_calls;
    model
        .update(cx, |model, cx| model.open_path("README.md", None, cx))
        .detach();
    wait_until(cx, |cx| tree_selection(&window, cx).as_deref() == Some("2"));
    cx.run_until_parked();
    assert_eq!(
        fake.state().read_blob_calls,
        reads + 1,
        "selecting the row opened nothing again"
    );
    model
        .update(cx, |model, cx| model.open_path("src/main.rs", None, cx))
        .detach();
    wait_until(cx, |cx| {
        opened(&window, cx).as_deref() == Some("src/main.rs")
    });
    assert_eq!(
        tree_selection(&window, cx),
        None,
        "a file inside a collapsed folder has no row to select"
    );
}

#[gpui::test]
fn the_tree_menu_copies_a_path_and_shows_its_history(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/a", repo())]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tree = cx.read(|cx| window.read(cx).sidebar().read(cx).tree().clone());
    tree.update(cx, |tree, cx| tree.copy_path("2", cx));
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("README.md"));
    tree.update(cx, |tree, cx| tree.show_history("1", cx));
    assert_eq!(cx.read(|cx| window.read(cx).mode()), ContentMode::History);
    let scope = cx.read(|cx| window.read(cx).model().read(cx).history_scope().clone());
    assert_eq!(
        scope,
        rc_git_kit::back_forward::HistoryScope::Path("src".to_owned())
    );
}

#[test]
fn only_rows_on_screen_count_toward_the_fit() {
    use rc_git_kit::file_tree::{ViewKind, ViewNode};
    let node = |label: &str, kind: ViewKind| ViewNode {
        id: label.to_owned(),
        label: label.to_owned(),
        kind,
    };
    let hidden = node("hidden-inside-collapsed", ViewKind::Placeholder);
    let shown = node("shown", ViewKind::Placeholder);
    let view = vec![
        node(
            "closed",
            ViewKind::Folder {
                expanded: false,
                children: vec![hidden],
            },
        ),
        node(
            "open",
            ViewKind::Folder {
                expanded: true,
                children: vec![shown],
            },
        ),
    ];
    let rows = crate::files::fit::extents(&view);
    let labels: Vec<(usize, &str, bool)> = rows
        .iter()
        .map(|row| (row.depth, row.label.as_str(), row.glyph))
        .collect();
    assert_eq!(
        labels,
        [(0, "closed", true), (0, "open", true), (1, "shown", false)]
    );
    let deeper = crate::files::fit::row_width(&rows[2], gpui::px(10.));
    let top = crate::files::fit::row_width(&rows[0], gpui::px(10.));
    assert_eq!(
        deeper - top,
        crate::style::FOLDER_INDENT - crate::style::ICON_SIZE - crate::style::ROW_GAP,
        "a level indents, a placeholder draws no glyph"
    );
}

#[gpui::test]
fn ctrl_r_opens_the_recent_picker_over_the_front_window(cx: &mut TestAppContext) {
    let harness = harness(cx, &[("/repo/alpha", repo())]);
    let window = open_settled(&harness, cx, "/repo/alpha");
    cx.update(|cx| {
        let bindings = crate::menus::key_bindings(cx).unwrap();
        cx.bind_keys(bindings);
    });
    cx.update(|cx| crate::register_actions(&harness.shell, cx));
    let tab = front(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_keystrokes("ctrl-r");
    assert_eq!(showing(&window, cx), Some(Showing::Recent));
}
