//! Phase 6's surfaces drawn through real CoreText and Metal off screen, over the fakes, so a
//! blank preview, panel or Settings window cannot ship.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, App, AppContext, Entity, HeadlessAppContext};
use rc_git_kit::fake::FakeRepository;
use rc_git_kit::platform::credentials::MemoryCredentialStore;
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::watch::ManualWatchers;

use crate::app::{Opened, Services, Shell, Tab};
use crate::tests::markdown::markdown_repo;
use crate::tests::support::count_ink;

pub fn settle(cx: &mut HeadlessAppContext, mut done: impl FnMut(&App) -> bool) {
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

/// A headless app whose shell opens `repo` at `/repo/a`, with `host_keys` for Settings.
pub fn headless(
    repo: FakeRepository,
    host_keys: Arc<MemoryHostKeyStore>,
) -> (HeadlessAppContext, Entity<Shell>) {
    let text_system = Arc::new(gpui_apple::AppleTextSystem::new());
    let mut cx =
        HeadlessAppContext::with_platform(text_system, Arc::new(crate::icons::Assets), || {
            gpui_platform::current_headless_renderer()
        });
    cx.update(gpui_component::init);
    let services = Services {
        settings: Arc::new(MemorySettings::new()),
        opener: Rc::new(move |path: &str| {
            Ok(Opened {
                reading: Arc::new(repo.clone()),
                key: format!("{path}/.git"),
                tab_path: path.to_owned(),
                dirs: None,
                actions: None,
            })
        }),
        watchers: Arc::new(ManualWatchers::new()),
        credentials: Arc::new(MemoryCredentialStore::new()),
        host_keys,
        home: "/Users/me".to_owned(),
        app_name: "rc-git".to_owned(),
    };
    let shell = cx.update(|cx| cx.new(|cx| Shell::new(services, cx)));
    (cx, shell)
}

/// Opens `/repo/a` and waits for its root tree.
pub fn open_window(cx: &mut HeadlessAppContext, shell: &Entity<Shell>) -> Tab {
    cx.update(|cx| shell.update(cx, |shell, cx| shell.open("/repo/a", cx)));
    let tab = cx.update(|cx| shell.read(cx).windows().first().cloned());
    let tab = tab.unwrap_or_else(|| panic!("the fake opens a window"));
    let model = cx.update(|cx| tab.view.read(cx).model().clone());
    settle(cx, |cx| model.read(cx).root_tree().is_some());
    tab
}

/// Dark and colored pixels of `window` right of `from` (a fraction of the width, in tenths).
pub fn ink_right_of(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tenths: u32,
) -> (usize, usize) {
    for _ in 0..3 {
        cx.run_until_parked();
    }
    let image = cx
        .capture_screenshot(window)
        .unwrap_or_else(|error| panic!("{error:#}"));
    let start = image.width() * tenths / 10;
    count_ink(
        image
            .enumerate_pixels()
            .filter(|(x, _, _)| *x > start)
            .map(|(_, _, pixel)| pixel.0),
    )
}

#[test]
fn the_markdown_preview_puts_ink_on_the_page() {
    let (mut cx, shell) = headless(markdown_repo(), Arc::new(MemoryHostKeyStore::new()));
    let tab = open_window(&mut cx, &shell);
    let model = cx.update(|cx| tab.view.read(cx).model().clone());
    cx.update(|cx| {
        model
            .update(cx, |model, cx| model.open_path("README.md", None, cx))
            .detach();
    });
    let pane = cx.update(|cx| tab.view.read(cx).pane().clone());
    settle(&mut cx, |cx| {
        pane.read(cx)
            .preview()
            .is_some_and(|preview| preview.read(cx).rendered().is_some())
    });
    let (ink, colored) = ink_right_of(&mut cx, tab.handle.into(), 3);
    assert!(
        ink > 3_000,
        "only {ink} dark pixels: the markdown preview is blank"
    );
    assert!(
        colored > 50,
        "only {colored} colored pixels: no link or highlighted fence"
    );
}
