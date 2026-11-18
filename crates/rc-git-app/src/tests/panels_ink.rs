//! Phase 6's surfaces drawn through real CoreText and Metal off screen, over the fakes, so a
//! blank preview, panel or Settings window cannot ship.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, App, AppContext, Entity, HeadlessAppContext};
use rc_git_kit::fake::FakeRepository;
use rc_git_kit::platform::api_tokens::MemoryApiTokenStore;
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
        api_tokens: Arc::new(MemoryApiTokenStore::new()),
        pull_requests: Arc::new(rc_git_kit::fake::FakePullRequests::with_link(
            rc_git_kit::pull_requests::link::RepositoryLink::Hidden,
        )),
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

/// Ink a panel adds over the window it opens on: a panel that draws nothing adds none.
fn panel_ink(
    cx: &mut HeadlessAppContext,
    tab: &Tab,
    show: impl FnOnce(
        &mut crate::window::RepoWindow,
        &mut gpui::Window,
        &mut gpui::Context<crate::window::RepoWindow>,
    ),
    ready: impl FnMut(&App) -> bool,
) -> usize {
    let (before, _) = ink_right_of(cx, tab.handle.into(), 0);
    let view = tab.view.clone();
    cx.update(|cx| {
        tab.handle
            .update(cx, |_, window, cx| {
                view.update(cx, |view, cx| show(view, window, cx));
            })
            .ok();
    });
    settle(cx, ready);
    let (after, _) = ink_right_of(cx, tab.handle.into(), 0);
    after.saturating_sub(before)
}

#[test]
fn the_go_to_file_panel_puts_ink_on_the_page() {
    let repo = crate::tests::markdown::markdown_repo();
    repo.state().trees.insert(
        rc_git_kit::fake::oid(crate::tests::support::GIT_DIR_TREE),
        Vec::new(),
    );
    let (mut cx, shell) = headless(repo, Arc::new(MemoryHostKeyStore::new()));
    let tab = open_window(&mut cx, &shell);
    let view = tab.view.clone();
    let added = panel_ink(
        &mut cx,
        &tab,
        crate::window::RepoWindow::show_go_to_file,
        move |cx| {
            view.read(cx)
                .panels()
                .go_to_file()
                .is_some_and(|panel| !panel.read(cx).results(cx).is_empty())
        },
    );
    assert!(
        added > 1_000,
        "only {added} more dark pixels: the panel is blank"
    );
}

#[test]
fn the_recent_picker_puts_ink_on_the_page() {
    let (mut cx, shell) = headless(markdown_repo(), Arc::new(MemoryHostKeyStore::new()));
    let tab = open_window(&mut cx, &shell);
    let added = panel_ink(
        &mut cx,
        &tab,
        |view, window, cx| {
            let recents = vec!["/Users/me/code/alpha".to_owned(), "/srv/beta".to_owned()];
            view.show_recent_picker(recents, "/Users/me".to_owned(), window, cx);
        },
        |_| true,
    );
    assert!(
        added > 1_000,
        "only {added} more dark pixels: the picker is blank"
    );
}

#[test]
fn the_settings_window_puts_ink_on_the_page() {
    let store = Arc::new(MemoryHostKeyStore::new());
    let approved = rc_git_kit::platform::host_keys::HostKeyStore::store(
        &*store,
        "SHA256:abcdefghijklmnopqrstuvwxyz0123456789",
        "github.com",
    );
    assert!(approved.is_ok());
    let (mut cx, shell) = headless(markdown_repo(), store);
    cx.update(|cx| shell.update(cx, Shell::show_settings));
    let handle = cx.update(|cx| shell.read(cx).settings_window().map(|(handle, _)| *handle));
    let handle = handle.unwrap_or_else(|| panic!("Settings opens a window"));
    let (ink, _) = ink_right_of(&mut cx, handle.into(), 0);
    assert!(ink > 2_000, "only {ink} dark pixels: Settings is blank");
}

#[test]
fn the_find_panel_puts_ink_on_the_page() {
    let (mut cx, shell) = headless(markdown_repo(), Arc::new(MemoryHostKeyStore::new()));
    let tab = open_window(&mut cx, &shell);
    let added = panel_ink(
        &mut cx,
        &tab,
        crate::window::RepoWindow::show_find_in_repository,
        |_| true,
    );
    assert!(
        added > 1_000,
        "only {added} more dark pixels: the panel is blank"
    );
}

/// A fake whose root holds a file named far wider than the sidebar's floor.
fn long_named_repo() -> FakeRepository {
    let repo = markdown_repo();
    {
        let mut state = repo.state();
        let root = rc_git_kit::fake::oid(crate::tests::support::MAIN_TREE);
        if let Some(entries) = state.trees.get_mut(&root) {
            entries.push(rc_git_core::TreeEntry {
                name: "a-file-name-long-enough-to-push-the-sidebar-wider.txt".into(),
                oid: rc_git_kit::fake::oid(crate::tests::support::README),
                kind: rc_git_core::EntryKind::Blob,
            });
        }
    }
    repo
}

#[test]
fn the_sidebar_fits_the_widest_tree_row_shaped_once() {
    let (mut cx, shell) = headless(long_named_repo(), Arc::new(MemoryHostKeyStore::new()));
    let tab = open_window(&mut cx, &shell);
    let view = tab.view.clone();
    settle(&mut cx, |cx| {
        view.read(cx).layout().width() > rc_git_kit::layout_policy::SIDEBAR_MINIMUM_WIDTH
    });
    let width = cx.update(|cx| view.read(cx).layout().width());
    assert!(width <= rc_git_kit::layout_policy::SIDEBAR_MAXIMUM_WIDTH);
    assert!(
        !cx.update(|cx| view.read(cx).layout().is_pinned()),
        "the fit's own resize is not a drag"
    );
    let tree = cx.update(|cx| view.read(cx).sidebar().read(cx).tree().clone());
    let shaped = cx.update(|cx| tree.read(cx).shaped_labels());
    for _ in 0..3 {
        cx.run_until_parked();
        let _ = cx.capture_screenshot(tab.handle.into());
    }
    assert_eq!(
        cx.update(|cx| tree.read(cx).shaped_labels()),
        shaped,
        "a repaint shapes no label again"
    );
}
