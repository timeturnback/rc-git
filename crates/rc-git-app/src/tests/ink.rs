//! A whole repository window, on the standard fixture, drawn through real CoreText and Metal
//! off screen, so a window that renders blank cannot ship.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, HeadlessAppContext};
use rc_git_kit::browser::FileState;
use rc_git_kit::platform::api_tokens::MemoryApiTokenStore;
use rc_git_kit::platform::credentials::MemoryCredentialStore;
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::watch::ManualWatchers;
use rc_git_testkit::Fixture;

use crate::app::{Services, Shell, live_opener};
use crate::tests::support::count_ink;

/// Runs the app until `done` holds; reads run on real threads, so this polls. The deadline
/// only bounds a hang.
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
fn a_repository_window_puts_ink_on_the_page() {
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
        pull_requests: Arc::new(rc_git_kit::fake::FakePullRequests::with_link(
            rc_git_kit::pull_requests::link::RepositoryLink::Hidden,
        )),
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
        model
            .update(cx, |model, cx| model.open_path("src/main.rs", None, cx))
            .detach();
    });
    settle(&mut cx, |cx| {
        matches!(model.read(cx).file_state(), FileState::Text(..))
    });
    let pane = cx.update(|cx| tab.view.read(cx).pane().clone());
    settle(&mut cx, |cx| {
        pane.read(cx)
            .viewer()
            .is_some_and(|viewer| viewer.read(cx).span_count() > 0)
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    let image = cx
        .capture_screenshot(tab.handle.into())
        .unwrap_or_else(|error| panic!("{error:#}"));
    let (ink, colored) = count_ink(image.pixels().map(|pixel| pixel.0));
    assert!(ink > 5_000, "only {ink} dark pixels: the window is blank");
    assert!(
        colored > 100,
        "only {colored} colored pixels: no icons or highlights"
    );
}
