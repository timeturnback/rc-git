//! Ported from `UpdateCopyTests.swift`: what the update window says and enables, per bundle
//! and state.

use rc_git_kit::app_build::AppBuild;
use rc_git_kit::update_copy::{UpdateCopy, UpdateState, UpdateStep};

fn build(source_root: Option<&str>) -> AppBuild {
    AppBuild {
        name: "rc-git".to_owned(),
        version: "0.1.0".to_owned(),
        build: "42".to_owned(),
        source_root: source_root.map(str::to_owned),
        commit: Some("a3f9".to_owned()),
        dirty: false,
        build_date: None,
    }
}

fn stamped() -> AppBuild {
    build(Some("/checkout"))
}

fn unstamped() -> AppBuild {
    build(None)
}

#[test]
fn the_installed_line_carries_the_local_build_date() {
    use rc_git_kit::app_build::BuildDate;
    use rc_git_kit::update_copy::installed_line;
    let mut stamped = stamped();
    assert_eq!(
        installed_line(&stamped, "Running", |_| 0),
        "Running: 0.1.0 (42) · a3f9"
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
fn the_title_names_the_bundle() {
    assert_eq!(
        UpdateCopy::new(&stamped(), &UpdateState::Idle).title,
        "Rebuild rc-git"
    );
}

#[test]
fn the_source_names_the_checkout() {
    let copy = UpdateCopy::new(&stamped(), &UpdateState::Idle);
    assert_eq!(
        copy.source,
        "Rebuilds /checkout as it stands now — no fetch, uncommitted work included."
    );
    let unstamped = UpdateCopy::new(&unstamped(), &UpdateState::Idle);
    assert!(unstamped.source.contains("cannot rebuild itself"));
}

#[test]
fn idle_offers_the_rebuild() {
    let copy = UpdateCopy::new(&stamped(), &UpdateState::Idle);
    assert_eq!(copy.status, "Rebuild and relaunch. Tabs come back.");
    assert_eq!(copy.action_title, "Rebuild and Relaunch");
    assert!(copy.action_enabled);
    assert!(!copy.spinning);
}

#[test]
fn an_unstamped_bundle_cannot_act() {
    assert!(!UpdateCopy::new(&unstamped(), &UpdateState::Idle).action_enabled);
    let state = failed(UpdateStep::Preflight, "no checkout");
    assert!(!UpdateCopy::new(&unstamped(), &state).action_enabled);
}

#[test]
fn running_spins_and_offers_cancel_for_the_build() {
    for step in [UpdateStep::Preflight, UpdateStep::Install] {
        let copy = UpdateCopy::new(&stamped(), &UpdateState::Running(step));
        assert!(copy.spinning, "{step:?}");
        assert_eq!(copy.status_tool_tip, None, "{step:?}");
    }
    let preflight = UpdateCopy::new(&stamped(), &UpdateState::Running(UpdateStep::Preflight));
    assert!(!preflight.action_enabled);
    assert!(!preflight.action_cancels);
    assert_eq!(preflight.status, "Checking the toolchain and the checkout…");
    let build = UpdateCopy::new(&stamped(), &UpdateState::Running(UpdateStep::Install));
    assert!(build.action_enabled);
    assert!(build.action_cancels);
    assert_eq!(build.action_title, "Cancel");
    assert_eq!(build.status, "Building the bundle — this takes a moment…");
}

#[test]
fn installed_spins_while_relaunching() {
    let copy = UpdateCopy::new(&stamped(), &UpdateState::Installed);
    assert_eq!(copy.status, "Built. Relaunching…");
    assert!(copy.spinning);
    assert!(!copy.action_enabled);
}

#[test]
fn a_failure_names_the_step_and_carries_the_full_message() {
    let copy = UpdateCopy::new(&stamped(), &failed(UpdateStep::Install, "linker error"));
    assert_eq!(copy.status, "Build failed: linker error");
    assert_eq!(copy.status_tool_tip.as_deref(), Some("linker error"));
    assert_eq!(copy.action_title, "Try Again");
    assert!(copy.action_enabled);
    assert!(!copy.spinning);
    let preflight = UpdateCopy::new(&stamped(), &failed(UpdateStep::Preflight, "y"));
    assert_eq!(preflight.status, "Preflight failed: y");
}

/// The bundle built and is in place; only the launch failed, so the copy does not blame the
/// build, and the launch is not something the button can cancel.
#[test]
fn the_relaunch_is_named_apart_from_the_build() {
    let running = UpdateCopy::new(&stamped(), &UpdateState::Running(UpdateStep::Relaunch));
    assert_eq!(running.status, "Launching the new build…");
    assert!(running.spinning);
    assert!(!running.action_cancels);
    assert!(!running.action_enabled);
    let failed = UpdateCopy::new(
        &stamped(),
        &failed(UpdateStep::Relaunch, "could not launch"),
    );
    assert_eq!(failed.status, "Relaunch failed: could not launch");
    assert_eq!(failed.action_title, "Try Again");
}

/// The tooltip belongs to the failure: every other state clears it.
#[test]
fn no_state_but_a_failure_has_a_tooltip() {
    for state in [
        UpdateState::Idle,
        UpdateState::Installed,
        UpdateState::Running(UpdateStep::Preflight),
    ] {
        assert_eq!(
            UpdateCopy::new(&stamped(), &state).status_tool_tip,
            None,
            "{state:?}"
        );
    }
}
