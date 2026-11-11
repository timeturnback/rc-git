//! The History pane on the standard fixture, drawn through real CoreText and Metal off screen
//! with a commit open: the list, the graph, the detail and the diff must all put ink down.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, HeadlessAppContext};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::models::FileDiffState;
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
