//! Port of `UpdateModel.swift`: the self-update. [`UpdateModel`] drives one rebuild of the
//! working tree as it stands — preflight, build, relaunch — and keeps a bounded log of it.
//! Everything it does outside the process goes through an [`UpdateEnvironment`]: the app
//! supplies the live one (`rc-git-mac`), tests a stub, so the state machine is tested without
//! building anything.
//!
//! The step and state types and the two button rules ([`is_cancellable`], [`can_proceed`])
//! live in [`crate::update_copy`], which the window reads; this module re-exports them.

mod inbox;
mod model;
mod run;

use std::sync::Arc;

use gpui::{App, Task};
use rc_git_core::{StatusReport, WorktreeStatus};

use crate::guard_bridge::CancelToken;

pub use crate::update_copy::{UpdateState, UpdateStep, can_proceed, is_cancellable};
pub use model::{
    DiagnosticLevel, FAILURE_TAIL_LINES, MAX_LOG_LINES, UpdateDiagnostic, UpdateEvent, UpdateModel,
};

/// The branch a checkout is on and the tracked paths that differ from its commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceState {
    /// Short branch name, or `None` on a detached HEAD. Logged before the build, because the
    /// rebuild ships whatever the checkout has checked out.
    pub branch: Option<String>,
    /// Modified or deleted tracked files, which go into the rebuild and are logged before it.
    /// Untracked and ignored files are not listed: `RCGit.app` and `build/` are build output.
    pub dirty_paths: Vec<String>,
    /// Whether the status scan covered the whole working tree. A scan stopped at its cap may
    /// have missed a dirty path, which the log says.
    pub scan_complete: bool,
}

impl SourceState {
    /// A state whose scan covered the whole tree.
    #[must_use]
    pub fn new(branch: Option<&str>, dirty_paths: &[&str]) -> Self {
        Self {
            branch: branch.map(str::to_owned),
            dirty_paths: dirty_paths.iter().map(|path| (*path).to_owned()).collect(),
            scan_complete: true,
        }
    }

    /// The state a working-tree status scan describes: its dirty paths, and whether it
    /// finished.
    #[must_use]
    pub fn from_report(branch: Option<&str>, report: &StatusReport) -> Self {
        Self {
            branch: branch.map(str::to_owned),
            dirty_paths: dirty_paths(report),
            scan_complete: !report.truncated,
        }
    }
}

/// The paths a rebuild treats as uncommitted work: modified or deleted tracked files.
#[must_use]
pub fn dirty_paths(report: &StatusReport) -> Vec<String> {
    let mut paths = Vec::new();
    for entry in &report.entries {
        match entry.status {
            WorktreeStatus::Modified | WorktreeStatus::Deleted => {
                paths.push(entry.path.to_string());
            }
            WorktreeStatus::Unchanged | WorktreeStatus::Untracked | WorktreeStatus::Ignored => {}
        }
    }
    paths
}

/// What reading a checkout produced: its state, or why it could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceStateResult {
    /// The checkout's state.
    State(SourceState),
    /// Why it could not be read.
    Failure(String),
}

/// What probing the toolchain produced.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ToolchainCheck {
    /// Every tool the build needs is on the login shell's `PATH`.
    Ready,
    /// Tools the login shell cannot find.
    Missing(Vec<String>),
    /// The probe itself did not answer: the shell could not run, it failed, or it ran past
    /// its limit and was killed. A probe that hangs is a failure, not a wait.
    Failure(String),
}

/// Where build output goes: one call per read of the script's output, from any thread.
pub type OutputSink = Arc<dyn Fn(Vec<String>) + Send + Sync>;

/// Everything a self-update touches outside the model: the toolchain, the checkout, the
/// build script, the tab hand-off and the relaunch.
///
/// Every call is made on the main thread and reports failure as a value rather than an
/// error: the model turns it into [`UpdateState::Failed`] with the step it belongs to. The
/// calls that return a [`Task`] run their work off the main thread and are bounded by the
/// implementation: each one answers rather than hanging.
pub trait UpdateEnvironment {
    /// Whether the login shell can find every tool the build needs.
    fn toolchain(&self, cx: &mut App) -> Task<ToolchainCheck>;
    /// The checkout's commit and dirty paths, or why they could not be read.
    fn source_state(&self, cx: &mut App) -> Task<SourceStateResult>;
    /// Rebuilds the bundle in the checkout with
    /// [`AppBuild::BUILD_SCRIPT`](crate::app_build::AppBuild::BUILD_SCRIPT), streaming the
    /// script's output to `on_output`. `None` on success. Stops — killing the build — once
    /// `cancel` is signalled.
    fn install(
        &self,
        cancel: Arc<CancelToken>,
        on_output: OutputSink,
        cx: &mut App,
    ) -> Task<Option<String>>;
    /// Hands the state the relaunched build reads — the open tabs — to it, and stops this
    /// process recording that state. Runs just before [`Self::relaunch`], so the new process
    /// finds the state already written when it starts.
    fn hand_off(&self, cx: &mut App);
    /// Takes back what [`Self::hand_off`] gave away, after a relaunch that failed: this
    /// process keeps running, so it records its state again.
    fn reclaim(&self, cx: &mut App);
    /// Launches the installed bundle. `None` on success. This instance keeps running, so the
    /// model can show [`UpdateState::Installed`] before [`Self::quit`].
    fn relaunch(&self, cx: &mut App) -> Task<Option<String>>;
    /// Quits this instance on a later turn, so the window draws
    /// [`UpdateState::Installed`] first. Runs only after [`Self::relaunch`] succeeded.
    fn quit(&self, cx: &mut App);
}

/// How far a view's copy of the log reaches: the run it shows, and how many of that run's
/// lines it has dropped from its head and taken in all, dropped ones included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogMark {
    /// The run the view shows.
    pub run: u64,
    /// Lines dropped from the head of that run's log.
    pub dropped: usize,
    /// Lines taken in all, dropped ones included.
    pub through: usize,
}

/// What a view showing the log at a [`LogMark`] does to catch up with the model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LogChange {
    /// Nothing new.
    None,
    /// A new run, or a view too far behind to patch: show these lines instead.
    Replace(Vec<String>),
    /// Delete `drop_head` lines from the head of what is shown, then add `lines` at the end.
    Append {
        /// Lines to delete from the head.
        drop_head: usize,
        /// Lines to add at the end.
        lines: Vec<String>,
    },
}

/// `a.rs, b.rs and 3 more` — enough to recognize the work without a wall of paths.
#[must_use]
pub fn list(paths: &[String], limit: usize) -> String {
    if paths.len() <= limit {
        return paths.join(", ");
    }
    let shown = paths[..limit].join(", ");
    format!("{shown} and {} more", paths.len() - limit)
}
