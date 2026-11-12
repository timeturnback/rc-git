//! The update and About windows drawn through real CoreText and Metal off screen, so a blank
//! one cannot ship.

use std::sync::Arc;

use rc_git_kit::app_build::{AppBuild, AppVariant};
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::update::UpdateState;

use crate::app::Shell;
use crate::tests::markdown::markdown_repo;
use crate::tests::panels_ink::{headless, ink_right_of, settle};
use crate::update::BuildInfo;

fn build() -> AppBuild {
    AppBuild {
        variant: AppVariant::Dev,
        name: "rc-git (dev)".to_owned(),
        version: "0.1.42".to_owned(),
        build: "42".to_owned(),
        source_root: Some("/nonexistent-rc-git-update-ink".to_owned()),
        commit: Some("a3f91c2d4e5f".to_owned()),
        dirty: true,
        build_date: rc_git_kit::app_build::BuildDate::parse("2026-09-19T11:02:03Z"),
    }
}

#[test]
fn the_update_window_puts_ink_on_the_page() {
    let (mut cx, shell) = headless(markdown_repo(), Arc::new(MemoryHostKeyStore::new()));
    cx.update(|cx| {
        cx.set_global(BuildInfo {
            build: build(),
            updatable: true,
        });
    });
    cx.update(|cx| shell.update(cx, Shell::rebuild_now));
    let model = cx.update(|cx| shell.read(cx).updater().map(crate::update::Updater::model));
    let model = model.unwrap_or_else(|| panic!("an update model"));
    settle(&mut cx, |cx| {
        matches!(model.read(cx).state(), UpdateState::Failed { .. })
    });
    let shown = cx.update(|cx| {
        shell
            .read(cx)
            .updater()
            .and_then(crate::update::Updater::shown)
    });
    let (handle, _) = shown.unwrap_or_else(|| panic!("the window opened"));
    let (ink, _) = ink_right_of(&mut cx, handle.into(), 0);
    assert!(
        ink > 2_000,
        "only {ink} dark pixels: the update window is blank"
    );
}

#[test]
fn the_about_window_puts_ink_on_the_page() {
    let (mut cx, _shell) = headless(markdown_repo(), Arc::new(MemoryHostKeyStore::new()));
    cx.update(|cx| crate::about::show(&build(), cx));
    let handle = cx.update(|cx| cx.windows().first().copied());
    let handle = handle.unwrap_or_else(|| panic!("About opens a window"));
    let (ink, _) = ink_right_of(&mut cx, handle, 0);
    assert!(ink > 500, "only {ink} dark pixels: About is blank");
}

#[test]
fn about_names_the_build() {
    let view = crate::about::AboutView::new(&build());
    let (name, version) = view.text();
    assert_eq!(name, "rc-git (dev)");
    assert!(
        version.starts_with("Version: 0.1.42 · a3f91c2+ · built Sep 19, 2026"),
        "{version}"
    );
}
