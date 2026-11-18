//! The Changes mode on the standard fixture with one file edited, drawn through real CoreText
//! and Metal off screen with that file's diff showing: the list, the tick boxes and the diff
//! must all put ink down.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, HeadlessAppContext};
use rc_git_kit::back_forward::ContentMode;
use rc_git_kit::changes::{ChangesState, WorktreeDiffState};
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

#[test]
fn the_changes_pane_puts_ink_on_the_page() {
    let fixture = Fixture::standard();
    fixture.write(
        "src/lib.rs",
        b"pub fn one() -> u32 {\n    2\n}\n\npub fn two() -> u32 {\n    2\n}\n",
    );
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
    cx.update(|cx| {
        tab.view
            .update(cx, |view, cx| view.switch_mode(ContentMode::Changes, cx));
    });
    let model = cx.update(|cx| tab.view.read(cx).changes().map(|parts| parts.model.clone()));
    let model = model.unwrap_or_else(|| panic!("the window has Changes models"));
    settle(
        &mut cx,
        |cx| matches!(model.read(cx).status_state(), ChangesState::Loaded(report) if !report.entries.is_empty()),
    );
    let entry = cx.update(|cx| match model.read(cx).status_state() {
        ChangesState::Loaded(report) => report.entries.first().cloned(),
        _ => None,
    });
    let entry = entry.unwrap_or_else(|| panic!("the edited file is listed"));
    cx.update(|cx| {
        model
            .update(cx, |model, cx| model.select(entry, cx))
            .detach();
    });
    settle(&mut cx, |cx| {
        matches!(model.read(cx).diff_state(), WorktreeDiffState::Loaded(..))
    });
    for _ in 0..3 {
        cx.run_until_parked();
    }
    let image = cx
        .capture_screenshot(tab.handle.into())
        .unwrap_or_else(|error| panic!("{error:#}"));
    // Right of the sidebar only, so the change list's ink cannot stand in for a blank diff.
    let sidebar_end = image.width() * 4 / 10;
    let pane = image
        .enumerate_pixels()
        .filter(|(x, _, _)| *x > sidebar_end)
        .map(|(_, _, pixel)| pixel.0);
    let (ink, colored) = count_ink(pane);
    assert!(
        ink > 2_000,
        "only {ink} dark pixels: the diff pane is blank"
    );
    assert!(
        colored > 100,
        "only {colored} colored pixels: no diff colors or tick boxes"
    );
    let (sidebar_ink, _) = count_ink(
        image
            .enumerate_pixels()
            .filter(|(x, y, _)| *x < sidebar_end && *y > image.height() / 10)
            .map(|(_, _, pixel)| pixel.0),
    );
    assert!(
        sidebar_ink > 1_000,
        "only {sidebar_ink} dark pixels: the change list is blank"
    );
}
