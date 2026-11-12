//! Port of `UpdateCopy.swift`, with the update states and the two rules from
//! `UpdateModel.swift` it reads: every string and control state the update window shows,
//! decided for one bundle and one [`UpdateState`]. The window assigns these fields and
//! decides nothing.

use crate::app_build::{AppBuild, UpdateMode};

/// One step of a self-update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateStep {
    /// Toolchain and checkout checks. Nothing has been touched yet.
    Preflight,
    /// Fetch and fast-forward the checkout, through the guard. [`UpdateMode::Rebuild`] skips
    /// it.
    Sync,
    /// Build the bundle and put it in place.
    Install,
    /// Hand the open tabs over and launch the new bundle. The bundle is already in place.
    Relaunch,
}

/// Where a self-update stands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UpdateState {
    /// Nothing started.
    Idle,
    /// A step is under way.
    Running(UpdateStep),
    /// The checkout is already at the commit this bundle was built from.
    UpToDate,
    /// The new bundle is in place and launched; this instance is about to quit.
    Installed,
    /// A step failed; `message` says why.
    Failed {
        /// The step that failed.
        step: UpdateStep,
        /// The full failure message.
        message: String,
    },
}

/// Whether the window's button cancels: only while the fetch or the build runs.
#[must_use]
pub fn is_cancellable(state: &UpdateState) -> bool {
    match state {
        UpdateState::Running(UpdateStep::Sync | UpdateStep::Install) => true,
        UpdateState::Running(UpdateStep::Preflight | UpdateStep::Relaunch)
        | UpdateState::Idle
        | UpdateState::UpToDate
        | UpdateState::Installed
        | UpdateState::Failed { .. } => false,
    }
}

/// Whether the window's button and its shortcut may start or restart an update: never for a
/// bundle with no checkout, and never while one runs or relaunches.
#[must_use]
pub fn can_proceed(build: &AppBuild, state: &UpdateState) -> bool {
    if build.update_mode().is_none() {
        return false;
    }
    match state {
        UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed { .. } => true,
        UpdateState::Running(_) | UpdateState::Installed => false,
    }
}

/// Every string and control state the update window shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpdateCopy {
    /// The window title.
    pub title: String,
    /// The word before the version line: what the version describes.
    pub heading: &'static str,
    /// What the button will build from.
    pub source: String,
    /// The line beside the button.
    pub status: String,
    /// The full failure message, which the status line may truncate; `None` outside
    /// [`UpdateState::Failed`], so no earlier failure lingers on a later state.
    pub status_tool_tip: Option<String>,
    /// The button's title.
    pub action_title: &'static str,
    /// Whether the button is enabled.
    pub action_enabled: bool,
    /// Whether the button cancels the running fetch or build; Return must not press it then.
    pub action_cancels: bool,
    /// Whether work is under way: running a step, or relaunching.
    pub spinning: bool,
}

impl UpdateCopy {
    /// The copy for `build` in `state`.
    #[must_use]
    pub fn new(build: &AppBuild, state: &UpdateState) -> Self {
        let rebuild = build.update_mode() == Some(UpdateMode::Rebuild);
        let action_cancels = is_cancellable(state);
        let status_tool_tip = match state {
            UpdateState::Failed { message, .. } => Some(message.clone()),
            UpdateState::Idle
            | UpdateState::Running(_)
            | UpdateState::UpToDate
            | UpdateState::Installed => None,
        };
        let spinning = match state {
            UpdateState::Running(_) | UpdateState::Installed => true,
            UpdateState::Idle | UpdateState::UpToDate | UpdateState::Failed { .. } => false,
        };
        Self {
            title: format!(
                "{} {}",
                if rebuild { "Rebuild" } else { "Update" },
                build.name
            ),
            heading: if rebuild { "Running" } else { "Installed" },
            source: source(build, rebuild),
            status: status(state, rebuild),
            status_tool_tip,
            action_title: if action_cancels {
                "Cancel"
            } else {
                action_title(state, rebuild)
            },
            action_enabled: action_cancels || can_proceed(build, state),
            action_cancels,
            spinning,
        }
    }
}

fn source(build: &AppBuild, rebuild: bool) -> String {
    let Some(root) = &build.source_root else {
        return "This bundle was not built by scripts/build-app.sh, so it cannot rebuild itself."
            .to_owned();
    };
    if rebuild {
        format!("Rebuilds {root} as it stands now — no fetch, uncommitted work included.")
    } else {
        format!("Builds from {root}, fast-forwarded from its upstream first.")
    }
}

fn status(state: &UpdateState, rebuild: bool) -> String {
    match state {
        UpdateState::Idle if rebuild => "Rebuild and relaunch. Tabs come back.".to_owned(),
        UpdateState::Idle => "Fetch, rebuild, install, relaunch. Tabs come back.".to_owned(),
        UpdateState::Running(step) => running(*step, rebuild).to_owned(),
        UpdateState::UpToDate => "Already at the commit this build came from.".to_owned(),
        UpdateState::Installed => {
            format!(
                "{}. Relaunching…",
                if rebuild { "Built" } else { "Installed" }
            )
        }
        UpdateState::Failed { step, message } => {
            format!("{} failed: {message}", step_name(*step, rebuild))
        }
    }
}

fn action_title(state: &UpdateState, rebuild: bool) -> &'static str {
    match state {
        UpdateState::UpToDate => "Rebuild Anyway",
        UpdateState::Failed { .. } => "Try Again",
        UpdateState::Idle | UpdateState::Running(_) | UpdateState::Installed => {
            if rebuild {
                "Rebuild and Relaunch"
            } else {
                "Update and Relaunch"
            }
        }
    }
}

fn running(step: UpdateStep, rebuild: bool) -> &'static str {
    match step {
        UpdateStep::Preflight => "Checking the toolchain and the checkout…",
        UpdateStep::Sync => "Fetching and fast-forwarding the checkout…",
        UpdateStep::Install if rebuild => "Building the dev bundle — this takes a moment…",
        UpdateStep::Install => "Building and installing — this takes a few minutes…",
        UpdateStep::Relaunch => "Launching the new build…",
    }
}

fn step_name(step: UpdateStep, rebuild: bool) -> &'static str {
    match step {
        UpdateStep::Preflight => "Preflight",
        UpdateStep::Sync => "Fetch",
        UpdateStep::Install if rebuild => "Build",
        UpdateStep::Install => "Install",
        UpdateStep::Relaunch => "Relaunch",
    }
}

/// The update window's header line: `Installed: 0.1.42 · a3f91c2 · built Nov 11, 2026 at
/// 3:04 PM`, the build date shown in the zone `offset_at` gives for that instant (seconds
/// east of UTC). A bundle with no readable build date leaves the date off.
#[must_use]
pub fn installed_line(build: &AppBuild, heading: &str, offset_at: impl Fn(i64) -> i32) -> String {
    let line = format!("{heading}: {}", build.version_line());
    let Some(date) = &build.build_date else {
        return line;
    };
    let instant = date.instant();
    let built = crate::dates::format_date_time(instant, offset_at(instant));
    format!("{line} · built {built}")
}
