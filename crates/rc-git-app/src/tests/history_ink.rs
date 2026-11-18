//! The History pane on the standard fixture, drawn through real CoreText and Metal off screen
//! with a commit open: the list, the graph, the detail and the diff must all put ink down.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, Focusable, HeadlessAppContext};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::models::FileDiffState;
use rc_git_kit::platform::api_tokens::MemoryApiTokenStore;
use rc_git_kit::platform::credentials::MemoryCredentialStore;
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::watch::ManualWatchers;
use rc_git_testkit::Fixture;

use crate::app::{Services, Shell, live_opener};
use crate::tests::support::count_ink;

fn settle(cx: &mut HeadlessAppContext, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if cx.update(|cx| done(cx)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the window"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Shows the open commit's Changes tab, where its diff draws, and lets it lay out.
fn show_changes_tab(cx: &mut HeadlessAppContext, tab: &crate::app::Tab) {
    cx.update(|cx| {
        let history = tab.view.read(cx).history().clone();
        history.update(cx, |history, cx| {
            history.set_detail_tab(crate::history::DetailTab::Changes, cx);
        });
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
}

#[test]
fn the_history_pane_puts_ink_on_the_page() {
    let fixture = Fixture::standard();
    let text_system = Arc::new(gpui_apple::AppleTextSystem::new());
    let mut cx =
        HeadlessAppContext::with_platform(text_system, Arc::new(crate::icons::Assets), || {
            gpui_platform::current_headless_renderer()
        });
    cx.update(gpui_component::init);
    let services = Services {
        settings: Arc::new(MemorySettings::new()),
        opener: live_opener(),
        watchers: Arc::new(ManualWatchers::new()),
        credentials: Arc::new(MemoryCredentialStore::new()),
        host_keys: Arc::new(MemoryHostKeyStore::new()),
        api_tokens: Arc::new(MemoryApiTokenStore::new()),
        home: String::new(),
        app_name: "rc-git".to_owned(),
    };
    let path = fixture.path().to_string_lossy().into_owned();
    let shell = cx.update(|cx| {
        let shell = cx.new(|cx| Shell::new(services, cx));
        shell.update(cx, |shell, cx| shell.open(&path, cx));
        shell
    });
    let tab = cx.update(|cx| shell.read(cx).windows().first().cloned());
    let tab = tab.unwrap_or_else(|| panic!("the fixture opens a window"));
    let model = cx.update(|cx| tab.view.read(cx).model().clone());
    settle(&mut cx, |cx| model.read(cx).root_tree().is_some());
    cx.update(|cx| {
        tab.view
            .update(cx, |view, cx| view.switch_mode(ContentMode::History, cx));
    });
    settle(&mut cx, |cx| !model.read(cx).history().is_empty());
    let first = cx.update(|cx| model.read(cx).history()[0].clone());
    cx.update(|cx| {
        model
            .update(cx, |model, cx| model.load_commit_changes(first, cx))
            .detach();
    });
    settle(&mut cx, |cx| {
        matches!(model.read(cx).file_diff(), FileDiffState::Loaded(..))
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    let image = cx
        .capture_screenshot(tab.handle.into())
        .unwrap_or_else(|error| panic!("{error:#}"));
    // Right of the sidebar only, so the ref list's ink cannot stand in for a blank pane.
    let sidebar_end = image.width() * 3 / 10;
    let pane = image
        .enumerate_pixels()
        .filter(|(x, _, _)| *x > sidebar_end)
        .map(|(_, _, pixel)| pixel.0);
    let (ink, colored) = count_ink(pane);
    assert!(
        ink > 5_000,
        "only {ink} dark pixels: the History pane is blank"
    );
    assert!(
        colored > 100,
        "only {colored} colored pixels: no graph, status letters or diff colors"
    );
}

#[test]
fn a_long_diff_line_scrolls_inside_the_history_pane() {
    let fixture = Fixture::standard();
    fixture.write("wide.txt", format!("{}\n", "wide ".repeat(800)).as_bytes());
    fixture.commit("Add a wide line", "2024-02-01T00:00:00Z");
    let text_system = Arc::new(gpui_apple::AppleTextSystem::new());
    let mut cx =
        HeadlessAppContext::with_platform(text_system, Arc::new(crate::icons::Assets), || {
            gpui_platform::current_headless_renderer()
        });
    cx.update(gpui_component::init);
    let services = Services {
        settings: Arc::new(MemorySettings::new()),
        opener: live_opener(),
        watchers: Arc::new(ManualWatchers::new()),
        credentials: Arc::new(MemoryCredentialStore::new()),
        host_keys: Arc::new(MemoryHostKeyStore::new()),
        api_tokens: Arc::new(MemoryApiTokenStore::new()),
        home: String::new(),
        app_name: "rc-git".to_owned(),
    };
    let path = fixture.path().to_string_lossy().into_owned();
    let shell = cx.update(|cx| {
        let shell = cx.new(|cx| Shell::new(services, cx));
        cx.set_global(crate::list_rows::WrapLines(false));
        shell.update(cx, |shell, cx| shell.open(&path, cx));
        shell
    });
    let tab = cx.update(|cx| shell.read(cx).windows().first().cloned());
    let tab = tab.unwrap_or_else(|| panic!("the fixture opens a window"));
    let model = cx.update(|cx| tab.view.read(cx).model().clone());
    settle(&mut cx, |cx| model.read(cx).root_tree().is_some());
    cx.update(|cx| {
        tab.view
            .update(cx, |view, cx| view.switch_mode(ContentMode::History, cx));
    });
    settle(&mut cx, |cx| !model.read(cx).history().is_empty());
    let first = cx.update(|cx| model.read(cx).history()[0].clone());
    cx.update(|cx| {
        model
            .update(cx, |model, cx| model.load_commit_changes(first, cx))
            .detach();
    });
    settle(&mut cx, |cx| {
        matches!(model.read(cx).file_diff(), FileDiffState::Loaded(..))
    });
    show_changes_tab(&mut cx, &tab);
    cx.capture_screenshot(tab.handle.into()).unwrap();
    let diff = cx.update(|cx| tab.view.read(cx).history().read(cx).diff().clone());
    let window_width = cx
        .update_window(tab.handle.into(), |_, window, _| window.bounds().size.width)
        .unwrap();
    let (content, viewport) = cx
        .update(|cx| diff.read(cx).row_scroll().scroll_widths())
        .unwrap();
    assert!(
        viewport < window_width,
        "the diff is {viewport:?} wide in a {window_width:?} window: it ran past the pane"
    );
    assert!(content > viewport, "{content:?} of content in {viewport:?}");
    let bounds = cx
        .update_window(tab.handle.into(), |_, window, _| window.bounds())
        .unwrap();
    let event = gpui::ScrollEvent {
        position: gpui::point(bounds.size.width * 0.8, bounds.size.height * 0.8),
        delta: gpui::ScrollDelta::Lines(gpui::point(-3., 0.)),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    };
    cx.update_window(tab.handle.into(), |_, window, cx| {
        window.dispatch_event(gpui::PlatformInput::Scroll(event), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.capture_screenshot(tab.handle.into()).unwrap();
    let offset = cx.update(|cx| diff.read(cx).row_scroll().scroll_offset());
    assert!(
        offset.x < gpui::px(0.),
        "a sideways wheel scrolled the diff: {offset:?}"
    );

    let focus = cx.update(|cx| diff.read(cx).focus_handle(cx));
    cx.update_window(tab.handle.into(), |_, window, cx| {
        focus.dispatch_action(&crate::menus::ToggleDiffExpanded, window, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.capture_screenshot(tab.handle.into()).unwrap();
    let (_, expanded) = cx
        .update(|cx| diff.read(cx).row_scroll().scroll_widths())
        .unwrap();
    assert!(
        cx.update(|cx| tab.view.read(cx).diff_expanded(cx)),
        "the path bar's action reached the window"
    );
    assert!(
        expanded > viewport && expanded > window_width * 0.9,
        "expanded, the diff is {expanded:?} of a {window_width:?} window (was {viewport:?})"
    );
}
