//! Ported from `UpdateCopyTests.swift`: what the update window says and enables, per bundle
//! and state.

use rc_git_kit::app_build::{AppBuild, AppVariant};
use rc_git_kit::update_copy::{UpdateCopy, UpdateState, UpdateStep};

fn build(variant: AppVariant, name: &str, source_root: Option<&str>) -> AppBuild {
    AppBuild {
        variant,
        name: name.to_owned(),
        version: "0.1.0".to_owned(),
        build: "42".to_owned(),
        source_root: source_root.map(str::to_owned),
        commit: Some("a3f9".to_owned()),
        dirty: false,
        build_date: None,
    }
}

fn release() -> AppBuild {
    build(AppVariant::Release, "rc-git", Some("/checkout"))
}

fn dev() -> AppBuild {
    build(AppVariant::Dev, "rc-git (dev)", Some("/checkout"))
}

fn unstamped() -> AppBuild {
    build(AppVariant::Release, "rc-git", None)
}

#[test]
fn the_installed_line_carries_the_local_build_date() {
    use rc_git_kit::app_build::BuildDate;
    use rc_git_kit::update_copy::installed_line;
    let mut stamped = release();
    assert_eq!(
        installed_line(&stamped, "Installed", |_| 0),
        "Installed: 0.1.0 (42) · a3f9"
    );
    stamped.build_date = BuildDate::parse("2026-09-19T11:02:03Z");
    let asked = std::cell::Cell::new(None);
    let line = installed_line(&stamped, "Running", |instant| {
        asked.set(Some(instant));
        -3_600
    });
    assert_eq!(
        line,
        "Running: 0.1.0 (42) · a3f9 · built Sep 19, 2026 at 10:02 AM"
    );
    assert_eq!(asked.get(), Some(1_789_815_723));
}

fn failed(step: UpdateStep, message: &str) -> UpdateState {
    UpdateState::Failed {
        step,
        message: message.to_owned(),
    }
}

#[test]
fn the_title_names_the_bundle_in_both_modes() {
    assert_eq!(
        UpdateCopy::new(&release(), &UpdateState::Idle).title,
        "Update rc-git"
    );
    assert_eq!(
        UpdateCopy::new(&dev(), &UpdateState::Idle).title,
        "Rebuild rc-git (dev)"
    );
}

#[test]
fn the_heading_and_source_follow_the_mode() {
    let release = UpdateCopy::new(&release(), &UpdateState::Idle);
    assert_eq!(release.heading, "Installed");
    assert_eq!(
        release.source,
        "Builds from /checkout, fast-forwarded from its upstream first."
    );
    let dev = UpdateCopy::new(&dev(), &UpdateState::Idle);
    assert_eq!(dev.heading, "Running");
    assert!(
        dev.source
            .starts_with("Rebuilds /checkout as it stands now")
    );
    let unstamped = UpdateCopy::new(&unstamped(), &UpdateState::Idle);
    assert!(unstamped.source.contains("cannot rebuild itself"));
}

#[test]
fn idle_offers_the_modes_action() {
    let release = UpdateCopy::new(&release(), &UpdateState::Idle);
    assert_eq!(
        release.status,
        "Fetch, rebuild, install, relaunch. Tabs come back."
    );
    assert_eq!(release.action_title, "Update and Relaunch");
    assert!(release.action_enabled);
    assert!(!release.spinning);
    let dev = UpdateCopy::new(&dev(), &UpdateState::Idle);
    assert_eq!(dev.status, "Rebuild and relaunch. Tabs come back.");
    assert_eq!(dev.action_title, "Rebuild and Relaunch");
    assert!(dev.action_enabled);
}

#[test]
fn an_unstamped_bundle_cannot_act() {
    assert!(!UpdateCopy::new(&unstamped(), &UpdateState::Idle).action_enabled);
    let state = failed(UpdateStep::Preflight, "no checkout");
    assert!(!UpdateCopy::new(&unstamped(), &state).action_enabled);
}

#[test]
fn running_spins_and_offers_cancel_for_the_fetch_and_the_build() {
    for build in [release(), dev()] {
        for step in [UpdateStep::Preflight, UpdateStep::Sync, UpdateStep::Install] {
            let copy = UpdateCopy::new(&build, &UpdateState::Running(step));
            assert!(copy.spinning, "{:?} {step:?}", build.variant);
            assert_eq!(copy.status_tool_tip, None, "{:?} {step:?}", build.variant);
        }
        let preflight = UpdateCopy::new(&build, &UpdateState::Running(UpdateStep::Preflight));
        assert!(!preflight.action_enabled);
        assert!(!preflight.action_cancels);
        for step in [UpdateStep::Sync, UpdateStep::Install] {
            let copy = UpdateCopy::new(&build, &UpdateState::Running(step));
            assert!(copy.action_enabled, "{:?} {step:?}", build.variant);
            assert!(copy.action_cancels, "{:?} {step:?}", build.variant);
            assert_eq!(copy.action_title, "Cancel", "{:?} {step:?}", build.variant);
        }
    }
    assert_eq!(
        UpdateCopy::new(&release(), &UpdateState::Running(UpdateStep::Install)).status,
        "Building and installing — this takes a few minutes…"
    );
    assert_eq!(
        UpdateCopy::new(&dev(), &UpdateState::Running(UpdateStep::Install)).status,
        "Building the dev bundle — this takes a moment…"
    );
    assert_eq!(
        UpdateCopy::new(&release(), &UpdateState::Running(UpdateStep::Sync)).status,
        "Fetching and fast-forwarding the checkout…"
    );
    assert_eq!(
        UpdateCopy::new(&release(), &UpdateState::Running(UpdateStep::Preflight)).status,
        "Checking the toolchain and the checkout…"
    );
}

#[test]
fn up_to_date_offers_a_rebuild_anyway() {
    let copy = UpdateCopy::new(&release(), &UpdateState::UpToDate);
    assert_eq!(copy.status, "Already at the commit this build came from.");
    assert_eq!(copy.action_title, "Rebuild Anyway");
    assert!(copy.action_enabled);
    assert!(!copy.spinning);
}

#[test]
fn installed_spins_while_relaunching() {
    let release = UpdateCopy::new(&release(), &UpdateState::Installed);
    assert_eq!(release.status, "Installed. Relaunching…");
    assert!(release.spinning);
    assert!(!release.action_enabled);
    assert_eq!(
        UpdateCopy::new(&dev(), &UpdateState::Installed).status,
        "Built. Relaunching…"
    );
}

#[test]
fn a_failure_names_the_step_and_carries_the_full_message() {
    let state = failed(UpdateStep::Install, "linker error");
    let copy = UpdateCopy::new(&release(), &state);
    assert_eq!(copy.status, "Install failed: linker error");
    assert_eq!(copy.status_tool_tip.as_deref(), Some("linker error"));
    assert_eq!(copy.action_title, "Try Again");
    assert!(copy.action_enabled);
    assert!(!copy.spinning);
    assert_eq!(
        UpdateCopy::new(&dev(), &state).status,
        "Build failed: linker error"
    );
    let fetch = UpdateCopy::new(&release(), &failed(UpdateStep::Sync, "x"));
    assert_eq!(fetch.status, "Fetch failed: x");
    let preflight = UpdateCopy::new(&release(), &failed(UpdateStep::Preflight, "y"));
    assert_eq!(preflight.status, "Preflight failed: y");
}

/// The bundle built and is in place; only the launch failed, so neither bundle blames the
/// build or the install, and the launch is not something the button can cancel.
#[test]
fn the_relaunch_is_named_apart_from_the_build() {
    for build in [release(), dev()] {
        let running = UpdateCopy::new(&build, &UpdateState::Running(UpdateStep::Relaunch));
        assert_eq!(
            running.status, "Launching the new build…",
            "{:?}",
            build.variant
        );
        assert!(running.spinning, "{:?}", build.variant);
        assert!(!running.action_cancels, "{:?}", build.variant);
        assert!(!running.action_enabled, "{:?}", build.variant);
        let failed = UpdateCopy::new(&build, &failed(UpdateStep::Relaunch, "could not launch"));
        assert_eq!(
            failed.status, "Relaunch failed: could not launch",
            "{:?}",
            build.variant
        );
        assert_eq!(failed.action_title, "Try Again", "{:?}", build.variant);
    }
}

/// The tooltip belongs to the failure: every other state clears it.
#[test]
fn no_state_but_a_failure_has_a_tooltip() {
    for state in [
        UpdateState::Idle,
        UpdateState::UpToDate,
        UpdateState::Installed,
        UpdateState::Running(UpdateStep::Preflight),
    ] {
        assert_eq!(
            UpdateCopy::new(&release(), &state).status_tool_tip,
            None,
            "{state:?}"
        );
    }
}
