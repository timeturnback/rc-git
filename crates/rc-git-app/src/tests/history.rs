//! The History pane's wiring against the fakes: one page at a time and only visible rows,
//! the commit detail and its diff, the search box, the History commands and their enablement,
//! the blame window, and the saved splits.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, IntoElement, TestAppContext, VisualTestContext};
use rc_git_core::{
    Blame, BlameRange, BlobDiff, ChangeKind, DiffHunk, DiffLine, DiffLineKind, EntryKind, LineEnd,
    Oid, TreeChange,
};
use rc_git_kit::back_forward::{ContentMode, HistoryScope};
use rc_git_kit::browser::{BrowserModel, FileState, HISTORY_PAGE_SIZE};
use rc_git_kit::fake::{FakeRepository, commit_summary, oid};
use rc_git_kit::layout_policy::{HISTORY_AUTHOR_WIDTH, HISTORY_DATE_MIN_WIDTH};
use rc_git_kit::platform::keys::{
    HISTORY_AUTHOR_COLUMN, HISTORY_COLUMN_SPLIT, HISTORY_DATE_COLUMN, HISTORY_SPLIT,
};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::settings::SettingsStore;
use rc_git_kit::platform::stores::SubviewFrame;
use rc_git_kit::window_geometry::{SplitAxis, split_frames};

use crate::app::Tab;
use crate::diff::{DiffContent, RowPrefix};
use crate::history::TYPING_PAUSE;
use crate::menus::{SearchCommits, ShowBlame, ShowFileHistory, ShowHistory};
use crate::tests::support::{
    Harness, MAIN_COMMIT, harness, harness_with, open_settled, repo, wait_until,
};
use crate::window::RepoWindow;

fn commit_oid(index: usize) -> Oid {
    let mut bytes = [0x70; 20];
    bytes[..8].copy_from_slice(&(index as u64).to_be_bytes());
    Oid::from_bytes(&bytes).unwrap()
}

/// [`repo`] with `count` commits under `main`, each the parent of the one before.
fn history_repo(count: usize) -> FakeRepository {
    let repo = repo();
    {
        let mut state = repo.state();
        let mut commits = Vec::with_capacity(count);
        for index in 0..count {
            let parents: Vec<Oid> = (index + 1 < count)
                .then(|| commit_oid(index + 1))
                .into_iter()
                .collect();
            commits.push(commit_summary(
                commit_oid(index),
                &parents,
                &format!("commit {index}"),
            ));
        }
        state.commits.insert(oid(MAIN_COMMIT), commits);
    }
    repo
}

fn only_tab(harness: &Harness, cx: &TestAppContext) -> Tab {
    let windows = harness
        .shell
        .read_with(cx, |shell, _| shell.windows().to_vec());
    assert_eq!(windows.len(), 1, "one window");
    windows[0].clone()
}

fn model(window: &Entity<RepoWindow>, cx: &TestAppContext) -> Entity<BrowserModel> {
    cx.read(|cx| window.read(cx).model().clone())
}

/// Opens `/repo/a` over `fake` and shows History.
fn open_history(
    cx: &mut TestAppContext,
    fake: FakeRepository,
) -> (Harness, Entity<RepoWindow>, Tab) {
    let harness = harness(cx, &[("/repo/a", fake)]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_tab(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(ShowHistory);
    visual.run_until_parked();
    (harness, window, tab)
}

fn modified(path: &str, old: u8, new: u8) -> TreeChange {
    TreeChange {
        kind: ChangeKind::Modified,
        path: path.into(),
        source_path: None,
        old: Some(oid(old)),
        new: Some(oid(new)),
        entry: EntryKind::Blob,
        old_entry: Some(EntryKind::Blob),
    }
}

fn hunks(lines: usize) -> Vec<DiffHunk> {
    let mut diff_lines = Vec::with_capacity(lines);
    for index in 0..lines {
        let kind = match index % 3 {
            0 => DiffLineKind::Context,
            1 => DiffLineKind::Removed,
            _ => DiffLineKind::Added,
        };
        diff_lines.push(DiffLine {
            kind,
            text: format!("line {index}").into(),
            eol: LineEnd::Lf,
        });
    }
    vec![DiffHunk {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: 1,
        lines: diff_lines,
    }]
}

#[gpui::test]
fn history_loads_one_page_and_builds_only_the_rows_on_screen(cx: &mut TestAppContext) {
    let fake = history_repo(HISTORY_PAGE_SIZE * 3);
    let (_harness, window, _tab) = open_history(cx, fake.clone());
    let model = model(&window, cx);
    wait_until(cx, |cx| {
        cx.read(|cx| model.read(cx).history().len()) == HISTORY_PAGE_SIZE
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    assert_eq!(
        cx.read(|cx| model.read(cx).history().len()),
        HISTORY_PAGE_SIZE,
        "rows on screen are far from the end, so no second page"
    );
    assert_eq!(fake.state().log_calls, 1, "one page, one read");
    let built = cx.read(|cx| window.read(cx).history().read(cx).built_rows());
    assert!(built > 0, "the list drew rows");
    assert!(
        built < HISTORY_PAGE_SIZE,
        "{built} rows built: only the rows on screen are"
    );
}

#[gpui::test]
fn a_short_history_draws_its_graph_and_asks_for_nothing_more(cx: &mut TestAppContext) {
    let fake = history_repo(3);
    let (_harness, window, _tab) = open_history(cx, fake.clone());
    let model = model(&window, cx);
    wait_until(cx, |cx| cx.read(|cx| model.read(cx).history_complete()));
    cx.run_until_parked();
    assert_eq!(fake.state().log_calls, 1);
    assert_eq!(cx.read(|cx| model.read(cx).graph_rows().len()), 3);
}

#[gpui::test]
fn an_open_commit_shows_its_files_and_the_first_diff(cx: &mut TestAppContext) {
    let fake = history_repo(3);
    {
        let mut state = fake.state();
        state.tree_diffs.insert(
            commit_oid(0),
            vec![modified("src/a.rs", 1, 2), modified("README.md", 3, 4)],
        );
        state.blob_diffs.insert(
            (Some(oid(1)), Some(oid(2))),
            BlobDiff::Text { hunks: hunks(30) },
        );
    }
    let (_harness, window, _tab) = open_history(cx, fake);
    let model = model(&window, cx);
    wait_until(cx, |cx| cx.read(|cx| model.read(cx).history_complete()));
    let first = cx.read(|cx| model.read(cx).history()[0].clone());
    model.update(cx, |model, cx| {
        model.load_commit_changes(first, cx).detach();
    });
    let pane = cx.read(|cx| window.read(cx).history().clone());
    let diff = cx.read(|cx| pane.read(cx).diff().clone());
    wait_until(cx, |cx| {
        cx.read(|cx| matches!(diff.read(cx).content(), DiffContent::Hunks(_)))
    });
    assert_eq!(
        cx.read(|cx| pane.read(cx).file_labels()),
        ["src", "  a.rs", "README.md"],
        "folders first, every folder open"
    );
    assert_eq!(
        cx.read(|cx| diff.read(cx).row_count()),
        31,
        "a header and 30 lines"
    );
    cx.run_until_parked();
    let built = cx.read(|cx| diff.read(cx).built_rows());
    assert!(built > 0, "the diff drew rows");
    let owner = cx.read(|cx| diff.read(cx).owning_hunk(5).map(|hunk| hunk.lines.len()));
    assert_eq!(owner, Some(30), "every row maps to its hunk");
}

#[gpui::test]
fn a_row_prefix_is_built_for_each_row_drawn(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let calls = Rc::new(Cell::new(0usize));
    let counted = Rc::clone(&calls);
    let prefix: RowPrefix = Rc::new(move |_, _, _| {
        counted.set(counted.get() + 1);
        gpui::div().into_any_element()
    });
    let window = cx.add_window(|_, cx| crate::diff::DiffView::new(cx));
    let view = window.root(cx).unwrap();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    view.update(&mut visual, |view, cx| {
        view.set_prefix(Some(prefix), cx);
        view.set_content(DiffContent::Hunks(hunks(5_000).into()), cx);
    });
    visual.run_until_parked();
    let built = visual.read(|cx| view.read(cx).built_rows());
    assert!(built > 0 && built < 5_001, "{built} rows built of 5001");
    assert_eq!(calls.get(), built, "one prefix per row built");
    let copied = visual.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.select_all(cx);
            view.selected_text()
        })
    });
    assert!(copied.starts_with("@@ -1,1 +1,1 @@\n line 0\n-line 1\n+line 2\n"));
}

#[gpui::test]
fn typing_searches_once_typing_pauses(cx: &mut TestAppContext) {
    let fake = history_repo(3);
    let (_harness, window, tab) = open_history(cx, fake);
    let model = model(&window, cx);
    wait_until(cx, |cx| cx.read(|cx| model.read(cx).history_complete()));
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(SearchCommits);
    visual.simulate_input("commit 1");
    visual.run_until_parked();
    assert_eq!(
        visual.read(|cx| model.read(cx).search_filter().cloned()),
        None,
        "nothing searches while typing"
    );
    visual.executor().advance_clock(TYPING_PAUSE);
    visual.run_until_parked();
    wait_until(cx, |cx| {
        cx.read(|cx| model.read(cx).search_filter().is_some() && !model.read(cx).is_searching())
    });
    let filter = cx.read(|cx| model.read(cx).search_filter().cloned());
    assert_eq!(
        filter.and_then(|filter| filter.message),
        Some("commit 1".to_owned())
    );
    assert_eq!(cx.read(|cx| model.read(cx).search_results().len()), 1);
}

#[gpui::test]
fn return_on_a_hex_word_jumps_to_that_revision(cx: &mut TestAppContext) {
    let fake = history_repo(3);
    {
        let mut state = fake.state();
        state.resolves.insert("abc1234".to_owned(), commit_oid(2));
        state.tree_diffs.insert(commit_oid(2), Vec::new());
    }
    let (_harness, window, tab) = open_history(cx, fake);
    let model = model(&window, cx);
    wait_until(cx, |cx| cx.read(|cx| model.read(cx).history_complete()));
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(SearchCommits);
    visual.simulate_input("abc1234");
    visual.simulate_keystrokes("enter");
    visual.run_until_parked();
    wait_until(cx, |cx| {
        cx.read(|cx| model.read(cx).diff_commit().map(|commit| commit.oid)) == Some(commit_oid(2))
    });
}

#[gpui::test]
fn the_file_commands_follow_the_open_file(cx: &mut TestAppContext) {
    let fake = history_repo(3);
    {
        let mut state = fake.state();
        state.path_histories.insert(
            (oid(MAIN_COMMIT), "README.md".to_owned()),
            vec![commit_summary(commit_oid(1), &[], "readme")],
        );
    }
    let harness = harness(cx, &[("/repo/a", fake)]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_tab(&harness, cx);
    let model = model(&window, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    let available = |visual: &mut VisualTestContext, action: &dyn gpui::Action| {
        visual.update(|window, cx| window.is_action_available(action, cx))
    };
    assert!(!available(&mut visual, &ShowBlame), "no file, no blame");
    assert!(!available(&mut visual, &ShowFileHistory));
    assert!(
        available(&mut visual, &SearchCommits),
        "⇧⌘F is Find in Repository in Files"
    );
    model.update(&mut visual, |model, cx| {
        model.open_path("README.md", None, cx).detach();
    });
    wait_until(cx, |cx| {
        cx.read(|cx| matches!(model.read(cx).file_state(), FileState::Text(..)))
    });
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.run_until_parked();
    assert!(available(&mut visual, &ShowBlame));
    assert!(available(&mut visual, &ShowFileHistory));
    visual.dispatch_action(ShowFileHistory);
    visual.run_until_parked();
    wait_until(cx, |cx| cx.read(|cx| model.read(cx).history_complete()));
    assert_eq!(cx.read(|cx| window.read(cx).mode()), ContentMode::History);
    assert_eq!(
        cx.read(|cx| model.read(cx).history_scope().clone()),
        HistoryScope::Path("README.md".to_owned())
    );
    assert_eq!(cx.read(|cx| model.read(cx).history().len()), 1);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    assert!(available(&mut visual, &SearchCommits));
}

#[gpui::test]
fn show_blame_opens_the_blame_window_with_its_ranges(cx: &mut TestAppContext) {
    let fake = history_repo(3);
    {
        let mut state = fake.state();
        state.blames.insert(
            "README.md".to_owned(),
            Blame::Ranges(vec![BlameRange {
                commit: commit_oid(1),
                start_line: 1,
                line_count: 3,
                boundary: false,
            }]),
        );
    }
    let harness = harness(cx, &[("/repo/a", fake)]);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_tab(&harness, cx);
    let model = model(&window, cx);
    model.update(cx, |model, cx| {
        model.open_path("README.md", None, cx).detach();
    });
    wait_until(cx, |cx| {
        cx.read(|cx| matches!(model.read(cx).file_state(), FileState::Text(..)))
    });
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.run_until_parked();
    visual.dispatch_action(ShowBlame);
    wait_until(cx, |cx| {
        cx.read(|cx| {
            window
                .read(cx)
                .blame()
                .view()
                .is_some_and(|view| view.read(cx).spans().len() == 1)
        })
    });
    assert_eq!(cx.windows().len(), 2, "blame is a second window");
    model.update(cx, BrowserModel::cancel_blame);
}

#[gpui::test]
fn the_splits_open_at_the_saved_sizes_and_save_a_drag(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    SubviewFrame::save_split(
        &*settings,
        HISTORY_SPLIT,
        &split_frames(SplitAxis::SideBySide, 500.0, 360.0, 700.0),
    )
    .unwrap();
    SubviewFrame::save_split(
        &*settings,
        HISTORY_COLUMN_SPLIT,
        &split_frames(SplitAxis::Stacked, 300.0, 250.0, 500.0),
    )
    .unwrap();
    let harness = harness_with(cx, &[("/repo/a", history_repo(3))], &settings);
    let window = open_settled(&harness, cx, "/repo/a");
    let tab = only_tab(&harness, cx);
    let pane = cx.read(|cx| window.read(cx).history().clone());
    assert_eq!(
        cx.read(|cx| pane.read(cx).saved_splits()),
        (Some(360.0), Some(250.0))
    );
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(ShowHistory);
    visual.run_until_parked();
    let outer = visual.read(|cx| pane.read(cx).outer_split().clone());
    outer.update_in(&mut visual, |state, window, cx| {
        state.resize_panel(0, gpui::px(520.), window, cx);
    });
    visual.run_until_parked();
    let saved = SubviewFrame::load_split(&*settings, HISTORY_SPLIT).unwrap();
    let dragged = visual.read(|cx| pane.read(cx).saved_splits().0);
    assert_eq!(
        dragged.map(f64::to_bits),
        Some(saved[1].frame.width.to_bits()),
        "the drag is saved under rc-git.history.v5"
    );
    let column = visual.read(|cx| f64::from(f32::from(outer.read(cx).sizes()[0])));
    assert_eq!(saved[0].frame.width.to_bits(), column.to_bits());
}

fn width_of(visual: &mut VisualTestContext, selector: &'static str) -> f64 {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    f64::from(f32::from(bounds.size.width))
}

#[gpui::test]
fn the_date_column_opens_fitted_and_a_drag_widens_and_saves_it(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[("/repo/a", history_repo(3))], &settings);
    open_settled(&harness, cx, "/repo/a");
    let tab = only_tab(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(ShowHistory);
    visual.run_until_parked();
    let fitted = width_of(&mut visual, "date-column");
    assert!(
        fitted > HISTORY_DATE_MIN_WIDTH,
        "fitted to a date: {fitted}"
    );
    assert_eq!(
        width_of(&mut visual, "author-column").to_bits(),
        HISTORY_AUTHOR_WIDTH.to_bits()
    );
    let handle = visual.debug_bounds("date-column-handle").unwrap().center();
    let to = handle - gpui::point(gpui::px(40.), gpui::px(0.));
    let none = gpui::Modifiers::none();
    visual.simulate_mouse_down(handle, gpui::MouseButton::Left, none);
    visual.simulate_mouse_move(
        handle - gpui::point(gpui::px(10.), gpui::px(0.)),
        gpui::MouseButton::Left,
        none,
    );
    visual.simulate_mouse_move(to, gpui::MouseButton::Left, none);
    visual.simulate_mouse_up(to, gpui::MouseButton::Left, none);
    visual.run_until_parked();
    let dragged = width_of(&mut visual, "date-column");
    assert_eq!(dragged.to_bits(), (fitted + 40.0).to_bits());
    assert_eq!(
        settings.double(HISTORY_DATE_COLUMN).to_bits(),
        dragged.to_bits(),
        "the drag is saved"
    );
    assert_eq!(
        settings.value(HISTORY_AUTHOR_COLUMN),
        None,
        "the author column was not dragged, so it keeps no width"
    );
}

#[gpui::test]
fn a_click_on_a_column_handle_saves_no_width(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[("/repo/a", history_repo(3))], &settings);
    open_settled(&harness, cx, "/repo/a");
    let tab = only_tab(&harness, cx);
    let mut visual = VisualTestContext::from_window(tab.handle.into(), cx);
    visual.dispatch_action(ShowHistory);
    visual.run_until_parked();
    let fitted = width_of(&mut visual, "date-column");
    let handle = visual.debug_bounds("date-column-handle").unwrap().center();
    let none = gpui::Modifiers::none();
    visual.simulate_mouse_down(handle, gpui::MouseButton::Left, none);
    visual.simulate_mouse_up(handle, gpui::MouseButton::Left, none);
    visual.run_until_parked();
    assert_eq!(
        width_of(&mut visual, "date-column").to_bits(),
        fitted.to_bits()
    );
    assert_eq!(
        settings.value(HISTORY_DATE_COLUMN),
        None,
        "a click is not a drag, so the column keeps following its fitted width"
    );
}
