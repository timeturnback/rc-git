//! The self-update's wiring against the fakes: which builds get an update, the window's log
//! following the model across a close and reopen, and the tab hand-off and reclaim through
//! the shell's own session.

use std::sync::Arc;

use gpui::TestAppContext;
use rc_git_kit::app_build::AppBuild;
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::SettingsTabStore;
use rc_git_kit::tabs::OpenTabs;
use rc_git_kit::update::{UpdateEnvironment, UpdateState, UpdateStep};
use rc_git_mac::update::LiveUpdateEnvironment;

use crate::app::{SessionGlobal, Shell};
use crate::tests::support::{harness_with, open_settled, repo, wait_until};
use crate::update::BuildInfo;

/// A checkout path nothing exists at, so a preflight fails before it spawns anything.
const GONE: &str = "/nonexistent-rc-git-update-checkout";

fn stamped_build(source_root: Option<&str>) -> AppBuild {
    AppBuild {
        name: "rc-git".to_owned(),
        version: "0.1.42".to_owned(),
        build: "42".to_owned(),
        source_root: source_root.map(str::to_owned),
        commit: Some("a3f91c2d4e5f".to_owned()),
        dirty: false,
        build_date: None,
    }
}

fn stamp(cx: &mut TestAppContext, build: AppBuild) {
    cx.update(|cx| cx.set_global(BuildInfo { build }));
}

#[gpui::test]
fn an_unstamped_build_has_no_update(cx: &mut TestAppContext) {
    let settings = Arc::new(MemorySettings::new());
    let harness = harness_with(cx, &[], &settings);
    let shown = harness.shell.update(cx, Shell::show_update);
    assert!(shown.is_none(), "no stamp, no update");

    stamp(cx, stamped_build(None));
    assert!(harness.shell.update(cx, Shell::show_update).is_none());
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
    stamp(cx, stamped_build(Some(GONE)));
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
    assert_eq!(runs(cx), 1, "Escape cancels only a running build");
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

    let hooks = harness.shell.update(cx, |shell, cx| shell.update_hooks(cx));
    let environment = LiveUpdateEnvironment::new(&stamped_build(Some(GONE)), hooks)
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
    stamp(cx, stamped_build(Some(GONE)));
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
