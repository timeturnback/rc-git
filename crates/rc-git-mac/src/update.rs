//! The live [`UpdateEnvironment`] (`UpdateEnvironmentLive.swift`): a login shell for the
//! toolchain probe and the build, the core for the checkout's state, a sync runner the app
//! supplies for the fetch and fast-forward (the same guarded pull the Changes tab runs), the
//! open-tab store for the hand-off, and `open -n` for the relaunch.
//!
//! The checkout and the script are rc-git's own: `scripts/install-app.sh` (release) or
//! `scripts/build-app.sh --dev` (dev) under the source root the bundle was stamped with, never
//! a browsed repository. A rebuild never reaches `sync`, so the dev loop needs no network and
//! no credentials.
//!
//! Every outside step goes through the bounded [`ProcessRunner`]: the probe has a timeout,
//! the build an idle clock, a wall-clock limit and a cancel, the relaunch a timeout, and each
//! kill takes the whole process group.

mod tabs;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Task};
use rc_git_kit::app_build::AppBuild;
use rc_git_kit::blocking::blocking;
use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::platform::process::{CommandSpec, Outcome, ProcessRunner, Stop};
use rc_git_kit::repository::{DEFAULT_STATUS_SCAN_CAP, LiveRepository, RepositoryReading};
use rc_git_kit::shell_word;
use rc_git_kit::update::{
    OutputSink, SourceState, SourceStateResult, ToolchainCheck, UpdateEnvironment, UpdateSyncResult,
};

use crate::process::ShellRunner;

pub use tabs::TabSession;

/// What the build needs and macOS does not ship. Once the Swift half is gone the build is
/// cargo alone; `git` is not probed, since the checkout is read through the core.
pub const REQUIRED_TOOLS: &[&str] = &["cargo"];

/// Prints each tool in [`REQUIRED_TOOLS`] that cannot run. `command -v` alone is not enough:
/// rustup's `cargo` proxy resolves with no toolchain behind it, so cargo has to answer
/// `--version`, with rustup's auto-install off: a missing pinned toolchain is a gap to name,
/// not a download to start inside the probe's timeout.
pub const TOOLCHAIN_PROBE: &str =
    "RUSTUP_AUTO_INSTALL=0 cargo --version >/dev/null 2>&1 || echo cargo";

/// The login, non-interactive `zsh` every outside step runs through, because an app launched
/// from the Dock inherits no shell `PATH`. That shell reads `.zshenv`, `.zprofile` and
/// `.zlogin` but not `.zshrc`, so a toolchain added to `PATH` only in `.zshrc` is not found.
pub const LOGIN_SHELL: &[&str] = &["/bin/zsh", "-lc"];

/// Runs `open`, which launches a bundle the way the Finder does.
const OPEN: &str = "/usr/bin/open";

/// How long each outside step may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// A probe that cannot answer in this long is stuck, not slow: it is one quick lookup in
    /// a login shell. Killing it turns a window that spins forever into one that names what
    /// to try in a terminal.
    pub toolchain: Duration,
    /// A build that prints nothing for this long is waiting, not working: cargo prints
    /// steadily, while one blocked on another build's lock prints a line and stops.
    pub build_idle: Duration,
    /// The most a build may take in all, idle or not: several times a cold release build.
    pub build: Duration,
    /// The most `open` may take to hand the bundle to Launch Services.
    pub relaunch: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            toolchain: Duration::from_secs(20),
            build_idle: Duration::from_mins(10),
            build: Duration::from_mins(45),
            relaunch: Duration::from_secs(30),
        }
    }
}

/// Fetches and fast-forwards the checkout at the given root: the app wires it to `SyncModel`
/// with its credential and host-key prompts. It stops early once the token is signalled.
pub type SyncRunner = Rc<dyn Fn(String, Arc<CancelToken>, &mut App) -> Task<UpdateSyncResult>>;

/// The repositories open in tabs now, in tab order, and the selected tab's index.
pub type LiveTabs = Rc<dyn Fn(&App) -> (Vec<String>, usize)>;

/// What the app hands the environment: the tab session it records through, the tabs on
/// screen, and the fetch.
#[derive(Clone)]
pub struct UpdateHooks {
    /// The open-tab session whose latch the hand-off sets and the reclaim lifts.
    pub tabs: Rc<RefCell<TabSession>>,
    /// The tabs on screen.
    pub live_tabs: LiveTabs,
    /// The fetch and fast-forward.
    pub sync: SyncRunner,
}

/// The real [`UpdateEnvironment`].
pub struct LiveUpdateEnvironment {
    source_root: String,
    script: (&'static str, &'static str),
    relaunch_path: String,
    hooks: UpdateHooks,
    runner: Arc<dyn ProcessRunner>,
    shell: Vec<String>,
    limits: Limits,
}

impl LiveUpdateEnvironment {
    /// The environment for `build`, or `None` for a bundle with no source checkout stamp,
    /// which cannot update itself (the model says so before calling anything).
    #[must_use]
    pub fn new(build: &AppBuild, hooks: UpdateHooks) -> Option<Self> {
        Some(Self {
            source_root: build.source_root.clone()?,
            script: build.update_mode().map(|_| build.variant.build_script())?,
            relaunch_path: build.relaunch_path()?,
            hooks,
            runner: Arc::new(ShellRunner),
            shell: LOGIN_SHELL.iter().map(|word| (*word).to_owned()).collect(),
            limits: Limits::default(),
        })
    }

    /// Runs every outside step through `runner`.
    #[must_use]
    pub fn with_runner(mut self, runner: Arc<dyn ProcessRunner>) -> Self {
        self.runner = runner;
        self
    }

    /// Runs the probe and the build as `shell` plus the script, instead of [`LOGIN_SHELL`].
    #[must_use]
    pub fn with_shell(mut self, shell: &[&str]) -> Self {
        self.shell = shell.iter().map(|word| (*word).to_owned()).collect();
        self
    }

    /// Bounds each step by `limits` instead of the defaults.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    fn steps(&self) -> Steps {
        Steps {
            source_root: self.source_root.clone(),
            script: self.script,
            relaunch_path: self.relaunch_path.clone(),
            runner: Arc::clone(&self.runner),
            shell: self.shell.clone(),
            limits: self.limits,
        }
    }
}

impl UpdateEnvironment for LiveUpdateEnvironment {
    fn toolchain(&self, cx: &mut App) -> Task<ToolchainCheck> {
        let steps = self.steps();
        blocking(cx.background_executor(), move || steps.toolchain())
    }

    fn source_state(&self, cx: &mut App) -> Task<SourceStateResult> {
        let root = self.source_root.clone();
        blocking(cx.background_executor(), move || source_state(&root))
    }

    fn sync(&self, cancel: Arc<CancelToken>, cx: &mut App) -> Task<UpdateSyncResult> {
        (self.hooks.sync)(self.source_root.clone(), cancel, cx)
    }

    fn install(
        &self,
        cancel: Arc<CancelToken>,
        on_output: OutputSink,
        cx: &mut App,
    ) -> Task<Option<String>> {
        let steps = self.steps();
        blocking(cx.background_executor(), move || {
            steps.install(&cancel, &on_output)
        })
    }

    fn hand_off(&self, cx: &mut App) {
        let (live, selected) = (self.hooks.live_tabs)(cx);
        self.hooks.tabs.borrow_mut().hand_off(&live, selected);
    }

    fn reclaim(&self, cx: &mut App) {
        let (live, selected) = (self.hooks.live_tabs)(cx);
        self.hooks.tabs.borrow_mut().reclaim(&live, selected);
    }

    fn relaunch(&self, cx: &mut App) -> Task<Option<String>> {
        let steps = self.steps();
        blocking(cx.background_executor(), move || steps.relaunch())
    }

    /// Quits a moment later, so the window draws the installed state first.
    fn quit(&self, cx: &mut App) {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(QUIT_DELAY).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

/// How long the window gets to draw the installed state before this instance quits.
pub const QUIT_DELAY: Duration = Duration::from_millis(250);

/// The parts of the environment the blocking steps need, owned so they can cross threads.
#[derive(Clone)]
pub(crate) struct Steps {
    /// The checkout the bundle was built from.
    pub(crate) source_root: String,
    /// The script the update runs, relative to the checkout, and its arguments.
    pub(crate) script: (&'static str, &'static str),
    /// The bundle to launch once the build finishes.
    pub(crate) relaunch_path: String,
    /// What runs every outside step.
    pub(crate) runner: Arc<dyn ProcessRunner>,
    /// The shell the probe and the build run in, before the script.
    pub(crate) shell: Vec<String>,
    /// The bound on each step.
    pub(crate) limits: Limits,
}

impl Steps {
    /// `script` run through the shell in the checkout, bounded by `timeout` and `idle`.
    fn shell_command(
        &self,
        script: &str,
        timeout: Duration,
        idle: Option<Duration>,
    ) -> CommandSpec {
        let (executable, arguments) = self.shell.split_first().map_or_else(
            || (String::new(), Vec::new()),
            |(first, rest)| (first.clone(), rest.to_vec()),
        );
        let mut arguments = arguments;
        arguments.push(script.to_owned());
        CommandSpec {
            executable,
            arguments,
            directory: self.source_root.clone(),
            timeout: Some(timeout),
            idle_timeout: idle,
        }
    }

    /// Whether the login shell can find every tool the build needs. Blocks; bounded by the
    /// probe's limit.
    #[must_use]
    pub(crate) fn toolchain(&self) -> ToolchainCheck {
        if !std::path::Path::new(&self.source_root).is_dir() {
            return ToolchainCheck::Failure(format!(
                "the checkout this bundle was built from is gone: {}; rebuild from its new \
                 location with scripts/install-app.sh",
                self.source_root
            ));
        }
        let command = self.shell_command(TOOLCHAIN_PROBE, self.limits.toolchain, None);
        let outcome = self.runner.run(&command, &CancelToken::new(), None);
        toolchain_check(&outcome, self.limits.toolchain)
    }

    /// Builds and installs the bundle, streaming its output to `on_output`. `None` on
    /// success. Blocks; bounded by the idle clock, the wall clock and `cancel`.
    #[must_use]
    pub(crate) fn install(&self, cancel: &CancelToken, on_output: &OutputSink) -> Option<String> {
        let (script, argument) = self.script;
        let path = shell_word::quote(&format!("{}/{script}", self.source_root));
        let command = self.shell_command(
            &format!("exec {path}{argument}"),
            self.limits.build,
            Some(self.limits.build_idle),
        );
        let forward = |lines: &[String]| on_output(lines.to_vec());
        let outcome = self.runner.run(&command, cancel, Some(&forward));
        install_failure(&outcome, &format!("{script}{argument}"), self.limits)
    }

    /// Launches the installed bundle as a new instance. `None` on success. Blocks; bounded by
    /// the relaunch limit.
    #[must_use]
    pub(crate) fn relaunch(&self) -> Option<String> {
        let mut command = CommandSpec::new(OPEN, &["-n", &self.relaunch_path], "/");
        command.timeout = Some(self.limits.relaunch);
        let outcome = self.runner.run(&command, &CancelToken::new(), None);
        if outcome.stopped.is_none() && outcome.succeeded() {
            return None;
        }
        let reason = match outcome.stopped {
            Some(_) => format!("did not answer in {}", duration(self.limits.relaunch)),
            None => outcome.failure(),
        };
        Some(format!(
            "built, but could not launch {}: open {reason}: {}",
            self.relaunch_path,
            last_line(&outcome)
        ))
    }
}

/// What a probe's outcome says about the toolchain.
fn toolchain_check(outcome: &Outcome, limit: Duration) -> ToolchainCheck {
    if outcome.stopped.is_some() {
        return ToolchainCheck::Failure(format!(
            "the toolchain check did not answer in {} and was killed; your login shell is \
             hanging — run `zsh -lc 'command -v {}'` in a terminal to see where",
            duration(limit),
            REQUIRED_TOOLS.first().copied().unwrap_or("cargo")
        ));
    }
    let mut missing = Vec::new();
    for line in &outcome.lines {
        if REQUIRED_TOOLS.contains(&line.as_str()) {
            missing.push(line.clone());
        }
    }
    if !missing.is_empty() {
        return ToolchainCheck::Missing(missing);
    }
    if !outcome.succeeded() {
        return ToolchainCheck::Failure(format!(
            "your login shell {} during the toolchain check: {}",
            outcome.failure(),
            last_line(outcome)
        ));
    }
    ToolchainCheck::Ready
}

/// Why a build failed, or `None` when it succeeded.
fn install_failure(outcome: &Outcome, script: &str, limits: Limits) -> Option<String> {
    let last = last_line(outcome);
    match outcome.stopped {
        Some(Stop::Idle) => Some(format!(
            "stopped after {} without output; last line: {last}",
            duration(limits.build_idle)
        )),
        Some(Stop::TimedOut) => Some(format!(
            "stopped after {}, the build's limit; last line: {last}",
            duration(limits.build)
        )),
        Some(Stop::Cancelled) => Some("cancelled".to_owned()),
        None if outcome.succeeded() => None,
        None => Some(format!("{script} {}: {last}", outcome.failure())),
    }
}

/// The last line that says anything, or "no output".
fn last_line(outcome: &Outcome) -> &str {
    outcome
        .lines
        .iter()
        .rev()
        .find(|line| !line.is_empty())
        .map_or("no output", String::as_str)
}

/// `45 min` for whole minutes, `20s` otherwise.
fn duration(limit: Duration) -> String {
    let seconds = limit.as_secs();
    if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

/// `refs/heads/master` as `master`; `None` on a detached HEAD.
#[must_use]
pub fn short_branch(ref_name: Option<&str>) -> Option<&str> {
    let name = ref_name?;
    Some(name.strip_prefix("refs/heads/").unwrap_or(name))
}

/// The checkout's commit, branch and dirty paths, read through the core. Blocks.
#[must_use]
pub fn source_state(root: &str) -> SourceStateResult {
    let read = || -> Result<SourceStateResult, String> {
        let repository = LiveRepository::open(root).map_err(|error| error.to_string())?;
        let head = repository.head().map_err(|error| error.to_string())?;
        let Some(commit) = head.target else {
            return Ok(SourceStateResult::Failure(format!(
                "the checkout at {root} has no commits"
            )));
        };
        let tree = repository
            .tree_of(commit)
            .map_err(|error| error.to_string())?;
        let report = repository
            .worktree_status(Some(tree), DEFAULT_STATUS_SCAN_CAP)
            .map_err(|error| error.to_string())?;
        Ok(SourceStateResult::State(SourceState::from_report(
            &commit.to_hex(),
            short_branch(head.ref_name.as_deref()),
            &report,
        )))
    };
    read().unwrap_or_else(|error| {
        SourceStateResult::Failure(format!("cannot read the checkout at {root}: {error}"))
    })
}

#[cfg(test)]
mod tests;
