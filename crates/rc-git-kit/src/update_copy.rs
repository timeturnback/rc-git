//! Port of `UpdateCopy.swift`, with the update states and the two rules from
//! `UpdateModel.swift` it reads: every string and control state the update window shows,
//! decided for one bundle and one [`UpdateState`]. The window assigns these fields and
//! decides nothing.

use crate::app_build::AppBuild;

/// One step of a self-update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateStep {
    /// Toolchain and checkout checks. Nothing has been touched yet.
    Preflight,
    /// Rebuild the bundle in the checkout.
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
    /// The new bundle is built and launched; this instance is about to quit.
    Installed,
    /// A step failed; `message` says why.
    Failed {
        /// The step that failed.
        step: UpdateStep,
        /// The full failure message.
        message: String,
    },
}

/// Whether the window's button cancels: only while the build runs.
#[must_use]
pub fn is_cancellable(state: &UpdateState) -> bool {
    match state {
        UpdateState::Running(UpdateStep::Install) => true,
        UpdateState::Running(UpdateStep::Preflight | UpdateStep::Relaunch)
        | UpdateState::Idle
        | UpdateState::Installed
        | UpdateState::Failed { .. } => false,
    }
}

/// Whether the window's button and its shortcut may start or restart an update: never for a
/// bundle with no checkout, and never while one runs or relaunches.
#[must_use]
pub fn can_proceed(build: &AppBuild, state: &UpdateState) -> bool {
    if !build.can_update() {
        return false;
    }
    match state {
        UpdateState::Idle | UpdateState::Failed { .. } => true,
        UpdateState::Running(_) | UpdateState::Installed => false,
    }
}

/// Every string and control state the update window shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpdateCopy {
    /// The window title.
    pub title: String,
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
    /// Whether the button cancels the running build; Return must not press it then.
    pub action_cancels: bool,
    /// Whether work is under way: running a step, or relaunching.
    pub spinning: bool,
}

impl UpdateCopy {
    /// The copy for `build` in `state`.
    #[must_use]
    pub fn new(build: &AppBuild, state: &UpdateState) -> Self {
        let action_cancels = is_cancellable(state);
        let status_tool_tip = match state {
            UpdateState::Failed { message, .. } => Some(message.clone()),
            UpdateState::Idle | UpdateState::Running(_) | UpdateState::Installed => None,
        };
        let spinning = match state {
            UpdateState::Running(_) | UpdateState::Installed => true,
            UpdateState::Idle | UpdateState::Failed { .. } => false,
        };
        Self {
            title: format!("Rebuild {}", build.name),
            source: source(build),
            status: status(state),
            status_tool_tip,
            action_title: if action_cancels {
                "Cancel"
            } else {
                action_title(state)
            },
            action_enabled: action_cancels || can_proceed(build, state),
            action_cancels,
            spinning,
        }
    }
}

fn source(build: &AppBuild) -> String {
    let Some(root) = &build.source_root else {
        return "This bundle was not built by scripts/build-app.sh, so it cannot rebuild itself."
            .to_owned();
    };
    format!("Rebuilds {root} as it stands now — no fetch, uncommitted work included.")
}

fn status(state: &UpdateState) -> String {
    match state {
        UpdateState::Idle => "Rebuild and relaunch. Tabs come back.".to_owned(),
        UpdateState::Running(step) => running(*step).to_owned(),
        UpdateState::Installed => "Built. Relaunching…".to_owned(),
        UpdateState::Failed { step, message } => {
            format!("{} failed: {message}", step_name(*step))
        }
    }
}

fn action_title(state: &UpdateState) -> &'static str {
    match state {
        UpdateState::Failed { .. } => "Try Again",
        UpdateState::Idle | UpdateState::Running(_) | UpdateState::Installed => {
            "Rebuild and Relaunch"
        }
    }
}

fn running(step: UpdateStep) -> &'static str {
    match step {
        UpdateStep::Preflight => "Checking the toolchain and the checkout…",
        UpdateStep::Install => "Building the bundle — this takes a moment…",
        UpdateStep::Relaunch => "Launching the new build…",
    }
}

fn step_name(step: UpdateStep) -> &'static str {
    match step {
        UpdateStep::Preflight => "Preflight",
        UpdateStep::Install => "Build",
        UpdateStep::Relaunch => "Relaunch",
    }
}

/// The update window's header line: `Running: 0.1.42 · a3f91c2 · built Nov 11, 2026 at
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
