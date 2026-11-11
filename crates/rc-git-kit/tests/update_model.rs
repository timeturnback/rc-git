//! Port of `UpdateModelTests.swift`, the runs that go start to finish: step order, the
//! up-to-date shortcut, every failure and what must not run after it, the two modes, the
//! log, and the dirty rule. The tests that hold a step open are in `update_model_held.rs`.

mod update_support;

use gpui::TestAppContext;
use rc_git_core::{EntryKind, StatusEntry, StatusReport, WorktreeStatus};
use rc_git_kit::app_build::AppVariant;
use rc_git_kit::update::{
    DiagnosticLevel, LogChange, MAX_LOG_LINES, SourceState, SourceStateResult, ToolchainCheck,
    UpdateDiagnostic, UpdateEvent, UpdateModel, UpdateState, UpdateStep, list,
};
use update_support::{
    HEAD, Script, build, clean, each_redraw_carries_one_change, events, frames, model, release,
    start,
};

fn failed(step: UpdateStep, message: &str) -> UpdateState {
    UpdateState::Failed {
        step,
        message: message.to_owned(),
    }
}

fn lines(prefix: &str, range: std::ops::Range<usize>) -> Vec<String> {
    range.map(|index| format!("{prefix}{index}")).collect()
}

fn log(model: &gpui::Entity<UpdateModel>, cx: &TestAppContext) -> Vec<String> {
    model.read_with(cx, |model, _| model.log().to_vec())
}

fn state(model: &gpui::Entity<UpdateModel>, cx: &TestAppContext) -> UpdateState {
    model.read_with(cx, |model, _| model.state().clone())
}

#[gpui::test]
async fn a_full_update_runs_every_step_in_order(cx: &mut TestAppContext) {
    let script = Script {
        install_output: vec!["==> building".to_owned(), "built RCGit.app".to_owned()],
        ..Script::synced()
    };
    let (model, stub) = model(cx, script, release());
    let seen = events(&model, cx);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    let frames = frames(&seen.borrow());
    assert_eq!(
        frames.last().map(|frame| &frame.0),
        Some(&UpdateState::Installed)
    );
    assert!(each_redraw_carries_one_change(&frames));
    assert_eq!(
        stub.calls(),
        [
            "toolchain",
            "sourceState",
            "sync",
            "sourceState",
            "install",
            "handOff",
            "relaunch",
            "quit"
        ]
    );
    let log = log(&model, cx);
    assert!(log.contains(&"on master".to_owned()));
    assert!(log.contains(&"built RCGit.app".to_owned()));
}

/// Quitting never returns, so the window has to read installed before the quit.
#[gpui::test]
async fn the_installed_state_is_shown_before_quitting(cx: &mut TestAppContext) {
    let (model, stub) = model(cx, Script::synced(), release());
    start(&model, false, cx).await;
    assert_eq!(stub.state_at_quit(), Some(UpdateState::Installed));
}

/// The commit did not move, so the minutes-long rebuild is skipped.
#[gpui::test]
async fn an_unchanged_checkout_stops_before_the_build(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    let seen = events(&model, cx);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::UpToDate);
    let frames = frames(&seen.borrow());
    assert_eq!(
        frames.last().map(|frame| &frame.0),
        Some(&UpdateState::UpToDate)
    );
    assert!(each_redraw_carries_one_change(&frames));
    assert!(!stub.calls().contains(&"install"));
}

/// A bundle built from uncommitted work holds code in no commit, so the same commit is no
/// reason to skip the rebuild.
#[gpui::test]
async fn a_dirty_build_at_the_same_commit_rebuilds(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        ..Script::default()
    };
    let mut dirty = release();
    dirty.dirty = true;
    let (model, stub) = model(cx, script, dirty);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert!(stub.calls().contains(&"install"));
}

/// A bundle stamped with no commit cannot prove it matches the checkout.
#[gpui::test]
async fn a_bundle_with_no_recorded_commit_rebuilds_instead_of_reporting_up_to_date(
    cx: &mut TestAppContext,
) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        ..Script::default()
    };
    let unstamped = build(AppVariant::Release, Some("/checkout"), None);
    let (model, stub) = model(cx, script, unstamped);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert!(stub.calls().contains(&"install"));
}

#[gpui::test]
async fn force_rebuilds_an_unchanged_checkout(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, true, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert!(stub.calls().contains(&"install"));
}

#[gpui::test]
async fn a_missing_tool_fails_preflight_without_touching_the_checkout(cx: &mut TestAppContext) {
    let script = Script {
        toolchain: ToolchainCheck::Missing(vec!["cargo".to_owned(), "cbindgen".to_owned()]),
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected a preflight failure");
    };
    assert_eq!(step, UpdateStep::Preflight);
    assert!(message.contains("cargo, cbindgen"));
    assert_eq!(stub.calls(), ["toolchain"]);
}

/// A probe that cannot answer is a failure with the reason, not a window that spins.
#[gpui::test]
async fn an_unanswerable_toolchain_check_fails_preflight(cx: &mut TestAppContext) {
    let reason = "the toolchain check did not answer in 20s and was killed";
    let script = Script {
        toolchain: ToolchainCheck::Failure(reason.to_owned()),
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), failed(UpdateStep::Preflight, reason));
    assert_eq!(stub.calls(), ["toolchain"]);
}

fn some(lines: &[&str]) -> Vec<Option<String>> {
    lines.iter().map(|line| Some((*line).to_owned())).collect()
}

/// Every step outside the process is logged before it runs: the stub reads the log as each
/// step starts, and a note moved after its await would leave the previous step's line there.
#[gpui::test]
async fn every_outside_step_is_logged_before_it_runs(cx: &mut TestAppContext) {
    let (model, stub) = model(cx, Script::synced(), release());
    stub.probe_last_line.set(true);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert_eq!(
        stub.last_lines(),
        some(&[
            "checking the toolchain",
            "reading the checkout",
            "fetching and fast-forwarding",
            "reading the checkout after the fetch",
            "building",
            "launching the new build",
            "launching the new build",
            "quitting this instance",
        ])
    );
}

/// The dev bundle names its build for what it is: the working tree, not a fetched commit.
#[gpui::test]
async fn a_rebuild_logs_the_working_tree_build_before_running_it(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        ..Script::default()
    };
    let dev = build(AppVariant::Dev, Some("/checkout"), Some(HEAD));
    let (model, stub) = model(cx, script, dev);
    stub.probe_last_line.set(true);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert_eq!(
        stub.last_lines(),
        some(&[
            "checking the toolchain",
            "reading the checkout",
            "rebuilding the working tree",
            "launching the new build",
            "launching the new build",
            "quitting this instance",
        ])
    );
}

#[gpui::test]
async fn uncommitted_work_stops_the_update(cx: &mut TestAppContext) {
    let dirty = SourceState::new(HEAD, Some("master"), &["a.rs", "b.rs", "c.rs", "d.rs"]);
    let script = Script {
        source_states: vec![SourceStateResult::State(dirty)],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected a preflight failure");
    };
    assert_eq!(step, UpdateStep::Preflight);
    assert!(message.contains("a.rs, b.rs, c.rs and 1 more"));
    assert!(!stub.calls().contains(&"sync"));
}

#[gpui::test]
async fn a_failed_fetch_stops_before_the_build(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        sync_failure: Some("host not trusted".to_owned()),
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    let seen = events(&model, cx);
    start(&model, false, cx).await;
    let expected = failed(UpdateStep::Sync, "host not trusted");
    assert_eq!(state(&model, cx), expected);
    let frames = frames(&seen.borrow());
    assert_eq!(frames.last().map(|frame| &frame.0), Some(&expected));
    assert!(each_redraw_carries_one_change(&frames));
    assert!(!stub.calls().contains(&"install"));
}

#[gpui::test]
async fn a_problem_the_pull_survived_is_logged_and_the_update_goes_on(cx: &mut TestAppContext) {
    let notice = "the credentials for github.com could not be saved: status -25308";
    let script = Script {
        sync_notice: Some(notice.to_owned()),
        ..Script::synced()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert!(log(&model, cx).contains(&format!("warning: {notice}")));
    assert!(stub.calls().contains(&"install"));
}

#[gpui::test]
async fn a_failed_build_never_relaunches(cx: &mut TestAppContext) {
    let script = Script {
        install_failure: Some("scripts/install-app.sh failed (status 1): linker error".to_owned()),
        install_output: vec!["==> building".to_owned(), "error: linker error".to_owned()],
        ..Script::synced()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected an install failure");
    };
    assert_eq!(step, UpdateStep::Install);
    assert!(message.contains("linker error"));
    assert!(!stub.calls().contains(&"relaunch"));
    assert!(log(&model, cx).contains(&"error: linker error".to_owned()));
}

/// A failed build reaches the app's log at error level, with the step and the last build
/// lines, so the evidence outlives the window and the process.
#[gpui::test]
async fn a_failed_build_is_logged_with_its_last_output(cx: &mut TestAppContext) {
    let marker = "c0ffee-marker";
    let script = Script {
        install_failure: Some(format!("linker error {marker}")),
        install_output: vec![format!("==> building {marker}")],
        ..Script::synced()
    };
    let (model, _) = model(cx, script, release());
    let seen = events(&model, cx);
    start(&model, false, cx).await;
    let mut errors = Vec::new();
    for event in seen.borrow().iter() {
        if let UpdateEvent::Diagnostic(UpdateDiagnostic {
            level: DiagnosticLevel::Error,
            message,
        }) = event
        {
            errors.push(message.clone());
        }
    }
    assert!(errors.contains(&format!("install failed: linker error {marker}")));
    assert!(errors.iter().any(|error| {
        error.starts_with("last build output:")
            && error.contains(&format!("\n==> building {marker}"))
    }));
}

/// The bundle is in place but the new instance did not come up: still a failure.
#[gpui::test]
async fn a_failed_relaunch_reports_the_relaunch_step(cx: &mut TestAppContext) {
    let script = Script {
        relaunch_failure: Some("could not launch /Applications/RCGit.app".to_owned()),
        ..Script::synced()
    };
    let (model, _) = model(cx, script, release());
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected a relaunch failure");
    };
    assert_eq!(step, UpdateStep::Relaunch);
    assert!(message.contains("/Applications/RCGit.app"));
}

/// The old process keeps running after a launch that failed, so the state it handed over is
/// taken back and recorded again.
#[gpui::test]
async fn a_failed_relaunch_reclaims_the_handed_off_state(cx: &mut TestAppContext) {
    let script = Script {
        relaunch_failure: Some("could not launch /Applications/RCGit.app".to_owned()),
        ..Script::synced()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    let calls = stub.calls();
    assert_eq!(calls[calls.len() - 3..], ["handOff", "relaunch", "reclaim"]);
    assert!(!calls.contains(&"quit"));
}

/// A build that never relaunches never hands its state over either.
#[gpui::test]
async fn a_failed_build_keeps_the_state(cx: &mut TestAppContext) {
    let script = Script {
        install_failure: Some("linker error".to_owned()),
        ..Script::synced()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    assert!(!stub.calls().contains(&"handOff"));
    assert!(!stub.calls().contains(&"reclaim"));
}

/// A read that fails after the fetch is the fetch step's failure, and nothing is built from
/// a checkout nobody could read.
#[gpui::test]
async fn an_unreadable_checkout_after_the_fetch_fails_the_sync_step(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD), SourceStateResult::Failure("x".to_owned())],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), failed(UpdateStep::Sync, "x"));
    assert!(!stub.calls().contains(&"install"));
}

#[gpui::test]
async fn an_unreadable_checkout_fails_preflight(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![SourceStateResult::Failure(
            "cannot open /checkout".to_owned(),
        )],
        ..Script::default()
    };
    let (model, _) = model(cx, script, release());
    start(&model, false, cx).await;
    assert_eq!(
        state(&model, cx),
        failed(UpdateStep::Preflight, "cannot open /checkout")
    );
}

/// The dev loop rebuilds the working tree as it stands: no fetch, uncommitted work goes in,
/// and no up-to-date shortcut, because the tree moves without the commit.
#[gpui::test]
async fn the_dev_bundle_rebuilds_the_working_tree_without_fetching(cx: &mut TestAppContext) {
    let dirty = SourceState::new(HEAD, Some("master"), &["a.rs"]);
    let script = Script {
        source_states: vec![SourceStateResult::State(dirty)],
        ..Script::default()
    };
    let dev = build(AppVariant::Dev, Some("/checkout"), Some(HEAD));
    let (model, stub) = model(cx, script, dev);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert_eq!(
        stub.calls(),
        [
            "toolchain",
            "sourceState",
            "install",
            "handOff",
            "relaunch",
            "quit"
        ]
    );
    let log = log(&model, cx);
    assert!(log.contains(&"uncommitted work goes in: a.rs".to_owned()));
    assert!(!log.iter().any(|line| line.contains("stopped at its cap")));
}

/// A rebuild after a scan that stopped at its cap says the dirty list it logged may be short.
#[gpui::test]
async fn a_rebuild_after_a_truncated_scan_says_the_list_is_partial(cx: &mut TestAppContext) {
    let mut partial = SourceState::new(HEAD, Some("master"), &["a.rs"]);
    partial.scan_complete = false;
    let script = Script {
        source_states: vec![SourceStateResult::State(partial)],
        ..Script::default()
    };
    let dev = build(AppVariant::Dev, Some("/checkout"), Some(HEAD));
    let (model, _) = model(cx, script, dev);
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    let log = log(&model, cx);
    assert!(log.contains(&"uncommitted work goes in: a.rs".to_owned()));
    assert!(log.contains(
        &"the status scan stopped at its cap, so more uncommitted work may go in than is listed"
            .to_owned()
    ));
}

#[gpui::test]
async fn a_dev_bundle_with_no_stamp_names_the_dev_script(cx: &mut TestAppContext) {
    let unstamped = build(AppVariant::Dev, None, Some(HEAD));
    let (model, stub) = model(cx, Script::default(), unstamped);
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected a preflight failure");
    };
    assert_eq!(step, UpdateStep::Preflight);
    assert!(message.contains("scripts/build-app.sh --dev"));
    assert!(stub.calls().is_empty());
}

#[gpui::test]
async fn an_unstamped_bundle_refuses_before_any_work(cx: &mut TestAppContext) {
    let unstamped = build(AppVariant::Release, None, Some(HEAD));
    let (model, stub) = model(cx, Script::default(), unstamped);
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected a preflight failure");
    };
    assert_eq!(step, UpdateStep::Preflight);
    assert!(message.contains("scripts/install-app.sh"));
    assert!(stub.calls().is_empty());
}

fn missing_cargo() -> Script {
    Script {
        toolchain: ToolchainCheck::Missing(vec!["cargo".to_owned()]),
        ..Script::default()
    }
}

#[gpui::test]
async fn reset_returns_a_failure_to_idle(cx: &mut TestAppContext) {
    let (model, _) = model(cx, missing_cargo(), release());
    start(&model, false, cx).await;
    model.update(cx, UpdateModel::reset);
    assert_eq!(state(&model, cx), UpdateState::Idle);
}

#[gpui::test]
async fn a_second_attempt_starts_with_a_fresh_log(cx: &mut TestAppContext) {
    let (model, _) = model(cx, missing_cargo(), release());
    start(&model, false, cx).await;
    model.update(cx, UpdateModel::reset);
    start(&model, false, cx).await;
    let probes = log(&model, cx)
        .iter()
        .filter(|line| *line == "checking the toolchain")
        .count();
    assert_eq!(probes, 1);
}

#[gpui::test]
async fn the_log_stays_bounded(cx: &mut TestAppContext) {
    let script = Script {
        install_output: lines("line ", 0..MAX_LOG_LINES + 500),
        ..Script::synced()
    };
    let (model, _) = model(cx, script, release());
    start(&model, false, cx).await;
    let log = log(&model, cx);
    assert_eq!(log.len(), MAX_LOG_LINES);
    assert_eq!(
        log[log.len() - 3..],
        [
            format!("line {}", MAX_LOG_LINES + 499),
            "launching the new build".to_owned(),
            "quitting this instance".to_owned(),
        ]
    );
}

/// How many times the window redraws while the build runs.
async fn redraws_during_install(script: Script, cx: &mut TestAppContext) -> (usize, Vec<String>) {
    let (model, _) = model(cx, script, release());
    let seen = events(&model, cx);
    start(&model, false, cx).await;
    let installing = UpdateState::Running(UpdateStep::Install);
    let redraws = frames(&seen.borrow())
        .iter()
        .filter(|frame| frame.0 == installing)
        .count();
    let output = log(&model, cx)
        .into_iter()
        .filter(|line| line.starts_with("line "))
        .collect();
    (redraws, output)
}

/// Ten reads of a hundred lines redraw the window about ten times, not a thousand.
#[gpui::test]
async fn build_output_redraws_once_per_read(cx: &mut TestAppContext) {
    let batches: Vec<Vec<String>> = (0..10)
        .map(|read| lines("line ", read * 100..read * 100 + 100))
        .collect();
    let script = Script {
        install_batches: batches.clone(),
        ..Script::synced()
    };
    let (redraws, output) = redraws_during_install(script, cx).await;
    assert_eq!(output, batches.concat());
    assert!(redraws <= 20, "{redraws} redraws");
}

/// A burst the build prints in one read costs one redraw, however many lines it holds.
#[gpui::test]
async fn a_burst_of_output_in_one_read_redraws_once(cx: &mut TestAppContext) {
    let burst = lines("line ", 0..500);
    let script = Script {
        install_output: burst.clone(),
        ..Script::synced()
    };
    let (redraws, output) = redraws_during_install(script, cx).await;
    assert_eq!(output, burst);
    assert!(redraws < 10, "{redraws} redraws");
}

fn apply(change: &LogChange, shown: &mut Vec<String>) {
    match change {
        LogChange::None => {}
        LogChange::Replace(lines) => shown.clone_from(lines),
        LogChange::Append { drop_head, lines } => {
            shown.drain(..(*drop_head).min(shown.len()));
            shown.extend(lines.iter().cloned());
        }
    }
}

/// The log a view shows, patched only through `log_change`.
#[derive(Default)]
struct View {
    shown: Vec<String>,
    mark: Option<rc_git_kit::update::LogMark>,
    changes: Vec<(u64, LogChange)>,
    mismatches: usize,
}

/// A view that only applies `log_change` shows the model's log at every redraw: through the
/// trim at the cap, and into a second run, which replaces the text. Events reach the view
/// after the update that emitted them, so a view that fell more than the cap behind (the
/// whole build output drained in one update) is replaced rather than patched; that is still
/// a match, and the first change of each run is always a replace.
#[gpui::test]
async fn a_view_patched_by_log_changes_matches_the_log(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let batches: Vec<Vec<String>> = (0..6)
        .map(|read| lines("line ", read * 1000..read * 1000 + 1000))
        .collect();
    let script = Script {
        install_batches: batches,
        ..Script::synced()
    };
    let (model, _) = model(cx, script, release());
    let view: Rc<RefCell<View>> = Rc::default();
    let drawn = Rc::clone(&view);
    cx.update(|cx| {
        cx.subscribe(&model, move |model, event, cx| {
            let UpdateEvent::Changed { .. } = event else {
                return;
            };
            let model = model.read(cx);
            let mut view = drawn.borrow_mut();
            let change = model.log_change(view.mark);
            view.mark = Some(model.log_mark());
            apply(&change, &mut view.shown);
            view.changes.push((model.log_run(), change));
            if view.shown != model.log() {
                view.mismatches += 1;
            }
        })
        .detach();
    });
    start(&model, false, cx).await;
    let first_run = model.read_with(cx, |model, _| model.log_run());
    start(&model, false, cx).await;
    assert_eq!(
        model.read_with(cx, |model, _| model.log_run()),
        first_run + 1
    );
    let view = view.borrow();
    assert_eq!(view.mismatches, 0);
    let mut run_starts = Vec::new();
    let mut previous_run = None;
    for (run, change) in &view.changes {
        if previous_run != Some(*run) {
            run_starts.push(change.clone());
            previous_run = Some(*run);
        }
    }
    assert_eq!(run_starts.len(), 2);
    assert!(
        run_starts
            .iter()
            .all(|change| matches!(change, LogChange::Replace(_)))
    );
    assert!(view.changes.iter().any(|(_, change)| matches!(
        change,
        LogChange::Append { drop_head, .. } if *drop_head > 0
    )));
}

#[gpui::test]
async fn proceed_from_idle_starts_the_update(cx: &mut TestAppContext) {
    let (model, stub) = model(cx, Script::synced(), release());
    assert!(model.read_with(cx, |model, _| model.can_proceed()));
    model.update(cx, UpdateModel::proceed).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert_eq!(stub.calls().first(), Some(&"toolchain"));
}

/// From up to date the button reads "Rebuild Anyway": it forces the build.
#[gpui::test]
async fn proceed_from_up_to_date_rebuilds_anyway(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![clean(HEAD)],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::UpToDate);
    assert!(model.read_with(cx, |model, _| model.can_proceed()));
    model.update(cx, UpdateModel::proceed).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    assert_eq!(
        stub.calls()
            .iter()
            .filter(|call| **call == "install")
            .count(),
        1
    );
}

#[gpui::test]
async fn proceed_from_a_failure_starts_over(cx: &mut TestAppContext) {
    let script = Script {
        source_states: vec![SourceStateResult::Failure(
            "cannot open /checkout".to_owned(),
        )],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    let expected = failed(UpdateStep::Preflight, "cannot open /checkout");
    assert_eq!(state(&model, cx), expected);
    assert!(model.read_with(cx, |model, _| model.can_proceed()));
    model.update(cx, UpdateModel::proceed).await;
    assert_eq!(
        stub.calls(),
        ["toolchain", "sourceState", "toolchain", "sourceState"]
    );
    assert_eq!(state(&model, cx), expected);
    let probes = log(&model, cx)
        .iter()
        .filter(|line| *line == "checking the toolchain")
        .count();
    assert_eq!(probes, 1);
}

#[gpui::test]
async fn proceed_after_install_does_nothing(cx: &mut TestAppContext) {
    let (model, stub) = model(cx, Script::synced(), release());
    start(&model, false, cx).await;
    assert_eq!(state(&model, cx), UpdateState::Installed);
    let calls_before = stub.calls();
    assert!(!model.read_with(cx, |model, _| model.can_proceed()));
    model.update(cx, UpdateModel::proceed).await;
    assert_eq!(stub.calls(), calls_before);
    assert_eq!(state(&model, cx), UpdateState::Installed);
}

#[gpui::test]
fn cancel_with_nothing_running_does_nothing(cx: &mut TestAppContext) {
    let (model, _) = model(cx, Script::default(), release());
    model.read_with(cx, |model, _| model.cancel());
    assert_eq!(state(&model, cx), UpdateState::Idle);
}

#[gpui::test]
async fn proceed_on_an_unstamped_bundle_does_nothing(cx: &mut TestAppContext) {
    let unstamped = build(AppVariant::Release, None, Some(HEAD));
    let (model, stub) = model(cx, Script::default(), unstamped);
    assert!(!model.read_with(cx, |model, _| model.can_proceed()));
    model.update(cx, UpdateModel::proceed).await;
    assert_eq!(state(&model, cx), UpdateState::Idle);
    assert!(stub.calls().is_empty());
}

fn entry(path: &str, status: WorktreeStatus) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        kind: EntryKind::Blob,
        status,
        oid: None,
        size: 0,
    }
}

fn report(entries: Vec<StatusEntry>, truncated: bool) -> StatusReport {
    StatusReport {
        entries,
        scanned: 10,
        truncated,
        ..StatusReport::default()
    }
}

#[test]
fn modified_and_deleted_files_are_dirty() {
    let report = report(
        vec![
            entry("a.rs", WorktreeStatus::Modified),
            entry("b.rs", WorktreeStatus::Deleted),
            entry("RCGit.app", WorktreeStatus::Untracked),
            entry("build/x", WorktreeStatus::Ignored),
        ],
        false,
    );
    let state = SourceState::from_report(HEAD, Some("master"), &report);
    assert_eq!(state.dirty_paths, ["a.rs", "b.rs"]);
    assert!(!state.is_clean());
}

#[test]
fn untracked_and_ignored_files_are_not_dirty() {
    let report = report(
        vec![
            entry("RCGit.app", WorktreeStatus::Untracked),
            entry("build/x", WorktreeStatus::Ignored),
        ],
        false,
    );
    let state = SourceState::from_report(HEAD, Some("master"), &report);
    assert!(state.dirty_paths.is_empty());
    assert!(state.is_clean());
}

#[test]
fn an_empty_report_is_clean() {
    assert!(SourceState::from_report(HEAD, None, &report(Vec::new(), false)).is_clean());
}

#[test]
fn a_truncated_report_is_not_clean() {
    let state = SourceState::from_report(HEAD, None, &report(Vec::new(), true));
    assert!(state.dirty_paths.is_empty());
    assert!(!state.scan_complete);
    assert!(!state.is_clean());
}

/// A scan that stopped at its cap may have missed uncommitted work, so the release update
/// refuses rather than fetching over it.
#[gpui::test]
async fn a_truncated_scan_stops_the_update(cx: &mut TestAppContext) {
    let mut partial = SourceState::new(HEAD, Some("master"), &[]);
    partial.scan_complete = false;
    let script = Script {
        source_states: vec![SourceStateResult::State(partial)],
        ..Script::default()
    };
    let (model, stub) = model(cx, script, release());
    start(&model, false, cx).await;
    let UpdateState::Failed { step, message } = state(&model, cx) else {
        panic!("expected a preflight failure");
    };
    assert_eq!(step, UpdateStep::Preflight);
    assert!(message.contains("uncommitted work cannot be ruled out"));
    assert!(!stub.calls().contains(&"sync"));
}

#[test]
fn list_names_up_to_three_paths() {
    let paths = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_owned()).collect() };
    assert_eq!(list(&paths(&["a", "b"]), 3), "a, b");
    assert_eq!(list(&paths(&["a", "b", "c"]), 3), "a, b, c");
    assert_eq!(list(&paths(&["a", "b", "c", "d"]), 3), "a, b, c and 1 more");
}
