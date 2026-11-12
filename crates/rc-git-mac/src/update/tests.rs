//! The live update environment against real `/bin/sh` scripts in a temporary checkout (never
//! the real build scripts), a fixture repository for the checkout read, and a scripted runner
//! for `open`. The login shell is replaced by `/bin/sh` with a `PATH` the test controls, so a
//! probe sees only the tools the test put there.
//!
//! Limits a child must outrun sit at seconds, far from the few seconds a spawn can take on a
//! loaded machine, as in the runner's own tests.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::TestAppContext;
use rc_git_kit::platform::process::{ScriptedRunner, Termination};
use rc_git_kit::platform::settings::MemorySettings;
use rc_git_kit::platform::stores::SharedSettings;
use rc_git_kit::tabs::{HandOff, TabStore};
use rc_git_testkit::Fixture;

use super::*;

/// A checkout with a `scripts` folder and an empty `bin` folder for fake tools, under a name
/// with a space so every path goes through the quoting.
struct Checkout {
    dir: Fixture,
    root: String,
}

/// The checkout's folder inside the temporary directory.
const CHECKOUT: &str = "rc git";

impl Checkout {
    fn new() -> Self {
        let dir = Fixture::empty_dir();
        dir.write(&format!("{CHECKOUT}/scripts/.keep"), b"");
        dir.write(&format!("{CHECKOUT}/bin/.keep"), b"");
        let root = dir.path().join(CHECKOUT).to_string_lossy().into_owned();
        Self { dir, root }
    }

    fn write_executable(&self, relative: &str, body: &str) {
        let relative = format!("{CHECKOUT}/{relative}");
        self.dir
            .write(&relative, format!("#!/bin/sh\n{body}\n").as_bytes());
        self.dir.set_mode(&relative, 0o755);
    }

    /// `/bin/sh` that runs `prefix`, then the script, with only `bin` and the system folders
    /// on `PATH`. The script arrives as `$0`.
    fn shell(&self, prefix: &str) -> Vec<String> {
        let path = shell_word::quote(&format!("{}/bin", self.root));
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("PATH={path}:/usr/bin:/bin; export PATH; {prefix} eval \"$0\""),
        ]
    }

    fn steps(&self, prefix: &str, limits: Limits) -> Steps {
        Steps {
            source_root: self.root.clone(),
            relaunch_path: format!("{}/RCGit.app", self.root),
            runner: Arc::new(ShellRunner),
            shell: self.shell(prefix),
            limits,
        }
    }
}

fn limits() -> Limits {
    Limits {
        toolchain: Duration::from_secs(20),
        build_idle: Duration::from_secs(20),
        build: Duration::from_secs(60),
        relaunch: Duration::from_secs(5),
    }
}

#[test]
fn a_cargo_that_answers_is_ready() {
    let checkout = Checkout::new();
    checkout.write_executable("bin/cargo", "echo 'cargo 1.98.0'");
    let steps = checkout.steps("", limits());
    assert_eq!(steps.toolchain(), ToolchainCheck::Ready);
}

#[test]
fn a_missing_cargo_is_named() {
    let checkout = Checkout::new();
    let steps = checkout.steps("", limits());
    assert_eq!(
        steps.toolchain(),
        ToolchainCheck::Missing(vec!["cargo".to_owned()])
    );
}

/// rustup's proxy resolves with no toolchain behind it, so `command -v` would pass it.
#[test]
fn a_cargo_proxy_with_no_toolchain_is_missing() {
    let checkout = Checkout::new();
    checkout.write_executable("bin/cargo", "echo 'error: no toolchain' >&2; exit 1");
    let steps = checkout.steps("", limits());
    assert_eq!(
        steps.toolchain(),
        ToolchainCheck::Missing(vec!["cargo".to_owned()])
    );
}

/// A login shell that hangs is killed at the probe's limit, and the failure says so.
#[test]
fn a_hanging_login_shell_fails_the_probe_at_its_limit() {
    let checkout = Checkout::new();
    let short = Limits {
        toolchain: Duration::from_secs(2),
        ..limits()
    };
    let steps = checkout.steps("exec sleep 30;", short);
    let ToolchainCheck::Failure(message) = steps.toolchain() else {
        panic!("expected the probe to fail");
    };
    assert!(
        message.starts_with("the toolchain check did not answer in 2s and was killed"),
        "{message}"
    );
}

#[test]
fn a_login_shell_that_fails_is_a_failure_with_its_last_line() {
    let checkout = Checkout::new();
    let steps = checkout.steps("echo 'bad .zprofile'; exit 3;", limits());
    assert_eq!(
        steps.toolchain(),
        ToolchainCheck::Failure(
            "your login shell failed (status 3) during the toolchain check: bad .zprofile"
                .to_owned()
        )
    );
}

#[test]
fn a_checkout_that_is_gone_fails_before_the_probe() {
    let checkout = Checkout::new();
    let mut steps = checkout.steps("", limits());
    steps.source_root = format!("{}/gone", checkout.root);
    let ToolchainCheck::Failure(message) = steps.toolchain() else {
        panic!("expected a failure");
    };
    assert!(message.contains("is gone"), "{message}");
}

/// Runs the build and collects what it streamed.
fn install(steps: &Steps, cancel: &CancelToken) -> (Option<String>, Vec<String>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let sink: OutputSink = Arc::new(move |lines| sink_seen.lock().unwrap().extend(lines));
    let result = steps.install(cancel, &sink);
    let lines = seen.lock().unwrap().clone();
    (result, lines)
}

#[test]
fn the_build_runs_build_app_with_no_arguments_and_streams_its_output() {
    let checkout = Checkout::new();
    checkout.write_executable(
        "scripts/build-app.sh",
        "echo \"==> building in $(pwd)\"; echo \"args: $#\"; echo built",
    );
    let steps = checkout.steps("", limits());
    let (result, lines) = install(&steps, &CancelToken::new());
    assert_eq!(result, None);
    assert_eq!(
        lines,
        [
            format!(
                "==> building in {}",
                std::fs::canonicalize(&checkout.root).unwrap().display()
            ),
            "args: 0".to_owned(),
            "built".to_owned()
        ]
    );
}

#[test]
fn a_failed_build_names_the_script_and_its_last_line() {
    let checkout = Checkout::new();
    checkout.write_executable(
        "scripts/build-app.sh",
        "echo '==> building'; echo 'error: linker error' >&2; echo; exit 1",
    );
    let steps = checkout.steps("", limits());
    let (result, _) = install(&steps, &CancelToken::new());
    assert_eq!(
        result.as_deref(),
        Some("scripts/build-app.sh failed (status 1): error: linker error")
    );
}

#[test]
fn a_build_that_goes_quiet_is_stopped_by_the_idle_clock() {
    let checkout = Checkout::new();
    checkout.write_executable(
        "scripts/build-app.sh",
        "echo 'waiting for the build lock'; exec sleep 60",
    );
    let quiet = Limits {
        build_idle: Duration::from_secs(3),
        ..limits()
    };
    let steps = checkout.steps("", quiet);
    let (result, _) = install(&steps, &CancelToken::new());
    assert_eq!(
        result.as_deref(),
        Some("stopped after 3s without output; last line: waiting for the build lock")
    );
}

#[test]
fn a_build_that_keeps_printing_is_stopped_by_the_wall_clock() {
    let checkout = Checkout::new();
    checkout.write_executable(
        "scripts/build-app.sh",
        "exec perl -e '$| = 1; while (1) { print \"tick\\n\"; select(undef, undef, undef, 0.2) }'",
    );
    let busy = Limits {
        build: Duration::from_secs(4),
        ..limits()
    };
    let steps = checkout.steps("", busy);
    let (result, _) = install(&steps, &CancelToken::new());
    assert_eq!(
        result.as_deref(),
        Some("stopped after 4s, the build's limit; last line: tick")
    );
}

/// The cancel lands from inside the running build, as the window's button does.
#[test]
fn a_cancelled_build_is_killed_and_says_cancelled() {
    let checkout = Checkout::new();
    checkout.write_executable("scripts/build-app.sh", "echo started; exec sleep 60");
    let steps = checkout.steps("", limits());
    let cancel = Arc::new(CancelToken::new());
    let from_output = Arc::clone(&cancel);
    let sink: OutputSink = Arc::new(move |_| from_output.signal());
    let started = std::time::Instant::now();
    assert_eq!(steps.install(&cancel, &sink).as_deref(), Some("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(30));
}

fn scripted(outcome: Outcome) -> (Steps, Arc<ScriptedRunner>) {
    let runner = Arc::new(ScriptedRunner::new());
    runner.push(outcome);
    let steps = Steps {
        source_root: "/checkout".to_owned(),
        relaunch_path: "/checkout/RCGit.app".to_owned(),
        runner: Arc::clone(&runner) as Arc<dyn ProcessRunner>,
        shell: LOGIN_SHELL.iter().map(|word| (*word).to_owned()).collect(),
        limits: limits(),
    };
    (steps, runner)
}

fn exited(status: i32, lines: &[&str]) -> Outcome {
    Outcome {
        status,
        termination: Termination::Exit,
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        stopped: None,
    }
}

#[test]
fn relaunch_opens_a_new_instance_of_the_bundle() {
    let (steps, runner) = scripted(exited(0, &[]));
    assert_eq!(steps.relaunch(), None);
    let commands = runner.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].executable, "/usr/bin/open");
    assert_eq!(commands[0].arguments, ["-n", "/checkout/RCGit.app"]);
    assert_eq!(commands[0].timeout, Some(limits().relaunch));
}

#[test]
fn a_launch_that_fails_says_the_bundle_is_built_but_not_running() {
    let (steps, _) = scripted(exited(
        1,
        &["LSOpenURLsWithRole() failed with error -10810"],
    ));
    assert_eq!(
        steps.relaunch().as_deref(),
        Some(
            "built, but could not launch /checkout/RCGit.app: open failed (status 1): \
             LSOpenURLsWithRole() failed with error -10810"
        )
    );
}

#[test]
fn a_launch_that_hangs_is_a_failure() {
    let mut hung = exited(15, &[]);
    hung.termination = Termination::Signal;
    hung.stopped = Some(Stop::TimedOut);
    let (steps, _) = scripted(hung);
    assert_eq!(
        steps.relaunch().as_deref(),
        Some(
            "built, but could not launch /checkout/RCGit.app: open did not answer in 5s: no output"
        )
    );
}

#[test]
fn the_checkout_is_read_through_the_core() {
    let fixture = Fixture::standard();
    let root = fixture.path().to_string_lossy().into_owned();
    assert_eq!(
        source_state(&root),
        SourceStateResult::State(SourceState::new(Some("main"), &[]))
    );
    fixture.write("README.md", b"changed\n");
    fixture.write("RCGit.app/Contents/Info.plist", b"build output\n");
    let SourceStateResult::State(state) = source_state(&root) else {
        panic!("expected a state");
    };
    assert_eq!(state.dirty_paths, ["README.md"]);
}

#[test]
fn a_detached_head_has_no_branch() {
    let fixture = Fixture::standard();
    fixture.git(&["checkout", "-q", "--detach", "HEAD"]);
    let root = fixture.path().to_string_lossy().into_owned();
    let SourceStateResult::State(state) = source_state(&root) else {
        panic!("expected a state");
    };
    assert_eq!(state.branch, None);
}

#[test]
fn a_checkout_that_is_not_a_repository_cannot_be_read() {
    let empty = Fixture::empty_dir();
    let root = empty.path().to_string_lossy().into_owned();
    let SourceStateResult::Failure(message) = source_state(&root) else {
        panic!("expected a failure");
    };
    assert!(
        message.starts_with(&format!("cannot read the checkout at {root}: ")),
        "{message}"
    );
}

#[test]
fn short_branch_drops_only_the_heads_prefix() {
    assert_eq!(short_branch(Some("refs/heads/master")), Some("master"));
    assert_eq!(short_branch(Some("refs/heads/feat/x")), Some("feat/x"));
    assert_eq!(
        short_branch(Some("refs/remotes/origin/x")),
        Some("refs/remotes/origin/x")
    );
    assert_eq!(short_branch(None), None);
}

fn paths(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn session() -> (TabSession, Arc<MemorySettings>) {
    let settings = Arc::new(MemorySettings::new());
    let shared: SharedSettings = Arc::clone(&settings) as SharedSettings;
    (TabSession::new(shared), settings)
}

/// After the hand-off the old process writes nothing: its write would land on top of what the
/// new process recorded.
#[test]
fn the_hand_off_flushes_the_tabs_and_latches_out_later_writes() {
    let (mut session, settings) = session();
    let handed = session.hand_off(&paths(&["/a", "/b"]), 1);
    assert_eq!(handed, HandOff::Flushed { count: 2 });
    assert_eq!(settings.synchronize_count(), 1);
    assert!(session.is_handed_off());
    assert!(!session.record(&paths(&["/c"]), 0, Some("/a")));
    assert_eq!(session.tabs().paths(), ["/a", "/b"]);
    assert_eq!(session.tabs().selected_index(), 1);
}

/// A launch that failed leaves this process running, so it records again, starting with the
/// tabs on screen now.
#[test]
fn reclaim_lifts_the_latch_and_records_the_live_tabs() {
    let (mut session, _) = session();
    session.hand_off(&paths(&["/a"]), 0);
    session.reclaim(&paths(&["/c", "/a"]), 0);
    assert!(!session.is_handed_off());
    assert_eq!(session.tabs().paths(), ["/c", "/a"]);
    assert!(session.record(&paths(&["/c"]), 0, Some("/a")));
    assert_eq!(session.tabs().paths(), ["/c"]);
}

#[test]
fn a_flush_that_fails_is_reported() {
    let (mut session, settings) = session();
    settings.fail_writes(true);
    assert_eq!(
        session.hand_off(&paths(&["/a"]), 0),
        HandOff::NotFlushed { count: 1 }
    );
    assert!(session.tabs().store().last_error().is_some());
}

fn hooks(tabs: Rc<RefCell<TabSession>>, live: Vec<String>) -> UpdateHooks {
    UpdateHooks {
        tabs,
        live_tabs: Rc::new(move |_| (live.clone(), 0)),
    }
}

#[test]
fn an_unstamped_bundle_has_no_environment() {
    let (session, _) = session();
    let tabs = Rc::new(RefCell::new(session));
    let mut build = AppBuild::from_info(&std::collections::HashMap::new());
    assert!(LiveUpdateEnvironment::new(&build, hooks(Rc::clone(&tabs), Vec::new())).is_none());
    build.source_root = Some("/checkout".to_owned());
    let environment = LiveUpdateEnvironment::new(&build, hooks(tabs, Vec::new())).unwrap();
    assert_eq!(environment.relaunch_path, "/checkout/RCGit.app");
    assert_eq!(environment.shell, LOGIN_SHELL);
}

/// The environment hands over the tabs on screen through the session, and takes them back.
#[gpui::test]
fn the_environment_hands_off_and_reclaims_the_live_tabs(cx: &mut TestAppContext) {
    let (session, _) = session();
    let tabs = Rc::new(RefCell::new(session));
    let mut build = AppBuild::from_info(&std::collections::HashMap::new());
    build.source_root = Some("/checkout".to_owned());
    let environment =
        LiveUpdateEnvironment::new(&build, hooks(Rc::clone(&tabs), paths(&["/x", "/y"]))).unwrap();
    cx.update(|cx| environment.hand_off(cx));
    assert!(tabs.borrow().is_handed_off());
    assert_eq!(tabs.borrow().tabs().paths(), ["/x", "/y"]);
    cx.update(|cx| environment.reclaim(cx));
    assert!(!tabs.borrow().is_handed_off());
}

#[test]
fn the_store_behind_the_session_is_the_settings_store() {
    let (session, settings) = session();
    let mut session = session;
    session.record(&paths(&["/a"]), 0, None);
    assert!(!settings.keys().is_empty());
    assert_eq!(session.tabs().store().paths(), ["/a"]);
}
