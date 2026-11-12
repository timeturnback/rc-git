//! The self-update's wiring against the fakes: which builds get an update, the window's log
//! following the model across a close and reopen, the tab hand-off and reclaim through the
//! shell's own session, and the update's pull — diverged, and cancelled from the update.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::TestAppContext;
use rc_git_kit::app_build::{AppBuild, AppVariant, UpdateMode};
use rc_git_kit::fake::{ActionKind, CallGate, FakeActions};
use rc_git_kit::guard_actions::{ActionError, FetchOutcome, UpstreamStatus};
use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::platform::credentials::MemoryCredentialStore;
use rc_git_kit::platform::host_keys::MemoryHostKeyStore;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::SettingsTabStore;
use rc_git_kit::platform::watch::ManualWatchers;
use rc_git_kit::tabs::OpenTabs;
use rc_git_kit::update::{UpdateEnvironment, UpdateState, UpdateStep, UpdateSyncResult};
use rc_git_mac::update::LiveUpdateEnvironment;

use crate::app::{Opened, Services, SessionGlobal, Shell};
use crate::tests::support::{harness_with, open_settled, repo, wait_until};
use crate::update::{BuildInfo, WindowSlot, sync_runner};

/// A checkout path nothing exists at, so a preflight fails before it spawns anything.
const GONE: &str = "/nonexistent-rc-git-update-checkout";

fn dev_build(source_root: Option<&str>) -> AppBuild {
    AppBuild {
        variant: AppVariant::Dev,
        name: "rc-git (dev)".to_owned(),
        version: "0.1.42".to_owned(),
        build: "42".to_owned(),
        source_root: source_root.map(str::to_owned),
        commit: Some("a3f91c2d4e5f".to_owned()),
        dirty: false,
        build_date: None,
    }
}

fn stamp(cx: &mut TestAppContext, build: AppBuild, updatable: bool) {
    cx.update(|cx| cx.set_global(BuildInfo { build, updatable }));
}

#[gpui::test]
fn an_unstamped_or_foreign_build_has_no_update(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[], &settings);
    let shown = harness.shell.update(cx, Shell::show_update);
    assert!(shown.is_none(), "no stamp, no update");

    stamp(cx, dev_build(None), true);
    assert!(harness.shell.update(cx, Shell::show_update).is_none());

    stamp(cx, dev_build(Some(GONE)), false);
    assert!(
        harness.shell.update(cx, Shell::show_update).is_none(),
        "the gpui bundle's update would build another bundle"
    );
    let mode = cx.read(|cx| harness.shell.read(cx).menu_state(cx).update_mode);
    assert_eq!(mode, Some(UpdateMode::Rebuild), "its items still show");
    assert!(
        harness
            .shell
            .read_with(cx, |shell, _| shell.updater().is_none())
    );
}

#[gpui::test]
fn rebuild_now_opens_the_window_and_its_log_follows_the_model(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[], &settings);
    stamp(cx, dev_build(Some(GONE)), true);
    harness.shell.update(cx, Shell::rebuild_now);
    let model = harness
        .shell
        .read_with(cx, |shell, _| {
            shell.updater().map(crate::update::Updater::model)
        })
        .unwrap_or_else(|| panic!("an update model"));
    wait_until(cx, |cx| {
        cx.read(|cx| matches!(model.read(cx).state(), UpdateState::Failed { .. }))
    });
    let state = cx.read(|cx| model.read(cx).state().clone());
    let UpdateState::Failed { step, message } = state else {
        panic!("a failed preflight");
    };
    assert_eq!(step, UpdateStep::Preflight);
    assert!(message.contains(GONE), "{message}");

    let shell = harness.shell.clone();
    let shown = |cx: &TestAppContext| {
        shell.read_with(cx, |shell, _| {
            shell.updater().and_then(crate::update::Updater::shown)
        })
    };
    let (handle, view) = shown(cx).unwrap_or_else(|| panic!("the window opened"));
    let log = cx.read(|cx| model.read(cx).log().to_vec());
    assert!(
        log.iter().any(|line| line.starts_with("error: ")),
        "{log:?}"
    );
    assert_eq!(cx.read(|cx| view.read(cx).lines()), log);

    let runs = |cx: &TestAppContext| cx.read(|cx| model.read(cx).log_run());
    assert_eq!(runs(cx), 1);
    let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(runs(cx), 1, "Escape cancels only a running fetch or build");
    let mut visual = gpui::VisualTestContext::from_window(handle.into(), cx);
    visual.simulate_keystrokes("enter");
    wait_until(cx, |cx| {
        runs(cx) == 2 && cx.read(|cx| matches!(model.read(cx).state(), UpdateState::Failed { .. }))
    });

    harness.shell.update(cx, |shell, cx| {
        shell.show_update(cx);
    });
    let (again, _) = shown(cx).unwrap_or_else(|| panic!("still open"));
    assert_eq!(again, handle, "one window serves every ask");

    handle
        .update(cx, |_, window, _| window.remove_window())
        .unwrap_or_else(|error| panic!("{error:#}"));
    cx.run_until_parked();
    harness.shell.update(cx, |shell, cx| {
        shell.show_update(cx);
    });
    let (reopened, view) = shown(cx).unwrap_or_else(|| panic!("reopened"));
    assert_ne!(reopened, handle);
    assert_eq!(
        cx.read(|cx| view.read(cx).lines()),
        log,
        "a new window shows the log the model kept"
    );
}

#[gpui::test]
fn the_hand_off_stops_saving_tabs_until_reclaimed(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[("/repo/a", repo()), ("/repo/b", repo())], &settings);
    open_settled(&harness, cx, "/repo/a");
    let saved = || OpenTabs::new(SettingsTabStore::new(settings.clone())).paths();
    assert_eq!(saved(), ["/repo/a"]);

    let hooks = harness.shell.update(cx, |shell, cx| {
        shell.update_hooks(WindowSlot::default(), cx)
    });
    let environment = LiveUpdateEnvironment::new(&dev_build(Some(GONE)), hooks)
        .unwrap_or_else(|| panic!("a stamped build has an environment"));
    cx.update(|cx| environment.hand_off(cx));
    assert!(cx.read(SessionGlobal::handed_off));
    assert_eq!(
        saved(),
        ["/repo/a"],
        "the hand-off wrote the tabs on screen"
    );

    open_settled(&harness, cx, "/repo/b");
    assert_eq!(
        saved(),
        ["/repo/a"],
        "nothing is recorded after the hand-off"
    );

    cx.update(|cx| environment.reclaim(cx));
    assert!(!cx.read(SessionGlobal::handed_off));
    assert_eq!(
        saved(),
        ["/repo/a", "/repo/b"],
        "a failed relaunch records again"
    );
}

#[gpui::test]
fn the_hand_off_stops_saving_the_window_frame(cx: &mut TestAppContext) {
    use rc_git_kit::platform::keys::MAIN_WINDOW_FRAME;
    use rc_git_kit::platform::stores::WindowFrame;
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[("/repo/a", repo())], &settings);
    open_settled(&harness, cx, "/repo/a");
    let tab = harness
        .shell
        .read_with(cx, |shell, _| shell.windows().first().cloned())
        .unwrap_or_else(|| panic!("a window"));
    let visual = gpui::VisualTestContext::from_window(tab.handle.into(), cx);
    let frame = || WindowFrame::load(&*settings, MAIN_WINDOW_FRAME);
    visual.simulate_resize(gpui::size(gpui::px(900.), gpui::px(600.)));
    visual.run_until_parked();
    let before = frame();
    assert!(before.is_some(), "a resize saves the frame");

    let session = harness.shell.read_with(cx, |shell, _| shell.tab_session());
    session.borrow_mut().hand_off(&["/repo/a".to_owned()], 0);
    let visual = gpui::VisualTestContext::from_window(tab.handle.into(), cx);
    visual.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(700.)));
    visual.run_until_parked();
    assert_eq!(frame(), before, "the relaunched build owns the frame now");
}

#[gpui::test]
fn the_saved_selection_ignores_a_window_that_is_not_a_tab(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[("/repo/a", repo()), ("/repo/b", repo())], &settings);
    open_settled(&harness, cx, "/repo/a");
    open_settled(&harness, cx, "/repo/b");
    let selected = |cx: &TestAppContext| cx.read(|cx| harness.shell.read(cx).tab_state(cx).1);
    assert_eq!(selected(cx), 1, "the tab opened last is in front");
    stamp(cx, dev_build(Some(GONE)), true);
    harness.shell.update(cx, |shell, cx| {
        shell.show_update(cx);
    });
    let handle = harness
        .shell
        .read_with(cx, |shell, _| {
            shell.updater().and_then(crate::update::Updater::shown)
        })
        .map_or_else(|| panic!("the update window opened"), |(handle, _)| handle);
    handle
        .update(cx, |_, window, _| window.activate_window())
        .unwrap_or_else(|error| panic!("{error:#}"));
    cx.run_until_parked();
    let update_is_front = cx.read(|cx| {
        let shown = harness
            .shell
            .read(cx)
            .updater()
            .and_then(crate::update::Updater::shown);
        cx.active_window().map(|window| window.window_id())
            == shown.map(|(handle, _)| handle.window_id())
    });
    assert!(update_is_front, "the update window took the front");
    assert_eq!(selected(cx), 1, "the tab behind it stays selected");
}

const CHECKOUT: &str = "/nonexistent-rc-git-update-source";

fn upstream() -> UpstreamStatus {
    UpstreamStatus {
        branch: "main".to_owned(),
        remote: "origin".to_owned(),
        upstream_ref: "refs/remotes/origin/main".to_owned(),
        url: "https://git.example.com/rc-git.git".to_owned(),
        host: "git.example.com".to_owned(),
        ahead: 1,
        behind: 1,
        has_upstream: true,
        published: true,
        uses_ssh: false,
    }
}

fn pull_services(actions: &FakeActions) -> Services {
    let actions = actions.clone();
    Services {
        settings: Arc::new(MemorySettings::new()),
        opener: Rc::new(move |path: &str| {
            if path != CHECKOUT {
                return Err(format!("{path} is not a repository"));
            }
            Ok(Opened {
                reading: Arc::new(repo()),
                key: format!("{path}/.git"),
                tab_path: path.to_owned(),
                dirs: None,
                actions: Some(Arc::new(actions.clone())),
            })
        }),
        watchers: Arc::new(ManualWatchers::new()),
        credentials: Arc::new(MemoryCredentialStore::new()),
        host_keys: Arc::new(MemoryHostKeyStore::new()),
        home: "/Users/me".to_owned(),
        app_name: "rc-git".to_owned(),
    }
}

fn pull_actions() -> FakeActions {
    let actions = FakeActions::new();
    {
        let mut state = actions.state();
        state.upstream = Some(upstream());
        state.fetched = Some(FetchOutcome {
            remote: "origin".to_owned(),
            updated_refs: Vec::new(),
        });
    }
    actions
}

/// Runs the update's pull of `root` and waits for its answer.
fn run_pull(
    cx: &mut TestAppContext,
    services: &Services,
    root: &str,
    cancel: &Arc<CancelToken>,
) -> Rc<RefCell<Option<UpdateSyncResult>>> {
    cx.executor().allow_parking();
    let runner = sync_runner(services, WindowSlot::default());
    let answer: Rc<RefCell<Option<UpdateSyncResult>>> = Rc::default();
    let slot = Rc::clone(&answer);
    let (root, cancel) = (root.to_owned(), Arc::clone(cancel));
    cx.update(|cx| {
        let task = runner(root, cancel, cx);
        cx.spawn(async move |_| {
            *slot.borrow_mut() = Some(task.await);
        })
        .detach();
    });
    answer
}

#[gpui::test]
fn a_diverged_checkout_fails_the_update_with_the_rebase(cx: &mut TestAppContext) {
    let actions = pull_actions();
    actions.state().fail_next.insert(
        ActionKind::PullFastForward,
        ActionError::Diverged {
            branch: "main".to_owned(),
            ahead: 1,
            behind: 1,
        },
    );
    let services = pull_services(&actions);
    let answer = run_pull(cx, &services, CHECKOUT, &Arc::new(CancelToken::new()));
    wait_until(cx, |_| answer.borrow().is_some());
    let Some(UpdateSyncResult::Failure(message)) = answer.borrow().clone() else {
        panic!("a diverged pull fails the update");
    };
    assert!(message.contains("git pull --rebase"), "{message}");
    assert!(message.contains(CHECKOUT), "{message}");
    assert_eq!(actions.state().count(ActionKind::Fetch), 1);
}

#[gpui::test]
fn a_checkout_that_does_not_open_fails_the_update(cx: &mut TestAppContext) {
    let services = pull_services(&pull_actions());
    let answer = run_pull(cx, &services, "/elsewhere", &Arc::new(CancelToken::new()));
    wait_until(cx, |_| answer.borrow().is_some());
    let Some(UpdateSyncResult::Failure(message)) = answer.borrow().clone() else {
        panic!("an unopened checkout fails");
    };
    assert!(
        message.contains("/elsewhere is not a repository"),
        "{message}"
    );
}

#[gpui::test]
fn the_updates_cancel_reaches_the_running_fetch(cx: &mut TestAppContext) {
    let actions = pull_actions();
    let gate = Arc::new(CallGate::new());
    actions
        .state()
        .gates
        .insert(ActionKind::Fetch, gate.clone());
    let services = pull_services(&actions);
    let cancel = Arc::new(CancelToken::new());
    let answer = run_pull(cx, &services, CHECKOUT, &cancel);
    wait_until(cx, |_| actions.state().count(ActionKind::Fetch) == 1);
    assert!(gate.wait_until_arrived(), "the fetch is running");
    cancel.signal();
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }
    gate.open();
    wait_until(cx, |_| answer.borrow().is_some());
    assert!(
        matches!(&*answer.borrow(), Some(UpdateSyncResult::Failure(_))),
        "{:?}",
        answer.borrow()
    );
    assert_eq!(
        actions.state().count(ActionKind::PullFastForward),
        0,
        "the fetch saw the cancel, so nothing was pulled"
    );
}

#[gpui::test]
fn a_cancel_before_the_pull_starts_fetches_nothing(cx: &mut TestAppContext) {
    let actions = pull_actions();
    let services = pull_services(&actions);
    let cancel = Arc::new(CancelToken::new());
    cancel.signal();
    let answer = run_pull(cx, &services, CHECKOUT, &cancel);
    wait_until(cx, |_| answer.borrow().is_some());
    assert_eq!(
        answer.borrow().clone(),
        Some(UpdateSyncResult::Failure(
            "the pull did not finish".to_owned()
        ))
    );
    assert_eq!(actions.state().count(ActionKind::Fetch), 0);
}

#[test]
fn the_shown_log_takes_each_change_in_place() {
    use std::collections::VecDeque;

    use gpui::SharedString;
    use rc_git_kit::update::LogChange;

    use crate::update::apply_log;

    let text = |lines: &VecDeque<SharedString>| -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    };
    let mut lines = VecDeque::new();
    apply_log(
        &mut lines,
        LogChange::Replace(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]),
    );
    assert_eq!(text(&lines), ["a", "b", "c"]);
    apply_log(
        &mut lines,
        LogChange::Append {
            drop_head: 2,
            lines: vec!["d".to_owned()],
        },
    );
    assert_eq!(text(&lines), ["c", "d"], "the trimmed head goes");
    apply_log(&mut lines, LogChange::None);
    assert_eq!(text(&lines), ["c", "d"]);
    apply_log(
        &mut lines,
        LogChange::Append {
            drop_head: 9,
            lines: vec!["e".to_owned()],
        },
    );
    assert_eq!(text(&lines), ["e"], "a trim past what is shown empties it");
}
