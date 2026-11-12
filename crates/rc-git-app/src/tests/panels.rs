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
