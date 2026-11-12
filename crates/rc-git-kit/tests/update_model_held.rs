//! Port of `UpdateModelTests.swift`, the tests that hold a step open: presses and starts
//! while a probe is held, and Cancel against a build that never returns.

mod update_support;

use std::rc::Rc;

use gpui::{Entity, TestAppContext};
use rc_git_kit::update::{UpdateEnvironment, UpdateModel, UpdateState, UpdateStep};
use update_support::{GatedToolchain, HangingBuild, model_over};

fn state(model: &Entity<UpdateModel>, cx: &TestAppContext) -> UpdateState {
    model.read_with(cx, |model, _| model.state().clone())
}

fn failed(step: UpdateStep, message: &str) -> UpdateState {
    UpdateState::Failed {
        step,
        message: message.to_owned(),
    }
}

fn gated(
    cx: &mut TestAppContext,
) -> (
    Entity<UpdateModel>,
    Rc<GatedToolchain>,
    rc_git_kit::oneshot::Sender<()>,
) {
    let (environment, release) = GatedToolchain::new();
    let shared: Rc<dyn UpdateEnvironment> = Rc::clone(&environment) as Rc<dyn UpdateEnvironment>;
    (model_over(cx, shared), environment, release)
}

/// A press while a step runs, the shortcut included, neither starts a second update nor
/// cancels the first.
#[gpui::test]
async fn proceed_while_running_does_nothing(cx: &mut TestAppContext) {
    let (model, environment, release) = gated(cx);
    let first = model.update(cx, UpdateModel::proceed);
    cx.run_until_parked();
    assert_eq!(
        state(&model, cx),
        UpdateState::Running(UpdateStep::Preflight)
    );
    assert!(!model.read_with(cx, |model, _| model.can_proceed()));
    model.update(cx, UpdateModel::proceed).await;
    assert_eq!(environment.toolchain_calls(), 1);
    release.send(());
    first.await;
    assert_eq!(environment.toolchain_calls(), 1);
}

/// A second start or a reset while an attempt runs is ignored: two builds never run into the
/// same staging folder, and the running attempt keeps its state and its log.
#[gpui::test]
async fn a_second_start_while_running_is_ignored(cx: &mut TestAppContext) {
    let (model, environment, release) = gated(cx);
    let first = model.update(cx, UpdateModel::start);
    cx.run_until_parked();
    assert!(model.read_with(cx, |model, _| model.is_busy()));
    model.update(cx, UpdateModel::start).await;
    model.update(cx, UpdateModel::reset);
    assert!(model.read_with(cx, |model, _| model.is_busy()));
    assert_eq!(
        state(&model, cx),
        UpdateState::Running(UpdateStep::Preflight)
    );
    let log = model.read_with(cx, |model, _| model.log().to_vec());
    assert!(log.contains(&"checking the toolchain".to_owned()));
    release.send(());
    first.await;
    assert_eq!(environment.toolchain_calls(), 1);
    assert_eq!(state(&model, cx), failed(UpdateStep::Preflight, "released"));
}

/// Preflight is a bounded probe: the button does not cancel it, and pressing it starts
/// nothing either.
#[gpui::test]
async fn press_action_during_preflight_does_nothing(cx: &mut TestAppContext) {
    let (model, environment, release) = gated(cx);
    let first = model.update(cx, UpdateModel::start);
    cx.run_until_parked();
    model.update(cx, UpdateModel::press_action).await;
    assert_eq!(
        state(&model, cx),
        UpdateState::Running(UpdateStep::Preflight)
    );
    release.send(());
    first.await;
    assert_eq!(state(&model, cx), failed(UpdateStep::Preflight, "released"));
    assert_eq!(environment.toolchain_calls(), 1);
}

fn hanging(cx: &mut TestAppContext) -> (Entity<UpdateModel>, Rc<HangingBuild>) {
    cx.executor().allow_parking();
    let environment = HangingBuild::new();
    let shared: Rc<dyn UpdateEnvironment> = Rc::clone(&environment) as Rc<dyn UpdateEnvironment>;
    (model_over(cx, shared), environment)
}

/// A build that never returns is stopped by Cancel: the update fails as cancelled, never
/// relaunches, and the next attempt runs clean.
#[gpui::test]
async fn cancel_stops_a_hung_build_and_the_next_attempt_runs_clean(cx: &mut TestAppContext) {
    let (model, environment) = hanging(cx);
    let first = model.update(cx, UpdateModel::start);
    cx.run_until_parked();
    assert_eq!(state(&model, cx), UpdateState::Running(UpdateStep::Install));
    model.update(cx, UpdateModel::press_action).await;
    first.await;
    assert_eq!(state(&model, cx), failed(UpdateStep::Install, "cancelled"));
    assert!(!environment.calls().contains(&"relaunch"));

    model.update(cx, UpdateModel::start).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert_eq!(environment.count("install"), 2);
    assert_eq!(environment.count("relaunch"), 1);
}

/// A line the cancelled build prints late stays out of the next attempt's log.
#[gpui::test]
async fn a_cancelled_builds_late_output_stays_out_of_the_next_log(cx: &mut TestAppContext) {
    let (model, environment) = hanging(cx);
    let first = model.update(cx, UpdateModel::start);
    cx.run_until_parked();
    model.read_with(cx, |model, _| model.cancel());
    first.await;
    model.update(cx, UpdateModel::start).await;
    let stale = environment.first_output().expect("the first build's sink");
    stale(vec!["late line from the cancelled build".to_owned()]);
    cx.run_until_parked();
    let log = model.read_with(cx, |model, _| model.log().to_vec());
    assert!(!log.contains(&"late line from the cancelled build".to_owned()));
    assert!(log.contains(&"quitting this instance".to_owned()));
}
