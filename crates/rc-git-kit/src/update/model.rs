//! The [`UpdateModel`] entity: its state, its bounded log, and the controls the window calls.
//! The update itself runs in [`super::run`].

use std::rc::Rc;
use std::sync::Arc;

use gpui::{Context, EventEmitter, Task};

use crate::app_build::AppBuild;
use crate::guard_bridge::CancelToken;
use crate::oneshot;

use super::inbox::OutputInbox;
use super::{
    LogChange, LogMark, UpdateEnvironment, UpdateState, UpdateStep, can_proceed, is_cancellable,
    run,
};

/// Lines the log keeps, like every other buffer in the app: a build log is not a reason to
/// grow without limit.
pub const MAX_LOG_LINES: usize = 5000;
/// How much build output a failed install copies into the error diagnostic. The rest of the
/// output goes out only at [`DiagnosticLevel::Info`].
pub const FAILURE_TAIL_LINES: usize = 20;

/// How durable a diagnostic should be, mirroring the unified log's levels: steps and state
/// changes at `Notice`, build output at `Info`, failures at `Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticLevel {
    /// Build output: worth keeping in memory, not on disk.
    Info,
    /// A step or a state transition.
    Notice,
    /// A failure.
    Error,
}

/// One line for the app's log (`os.Logger(category: "update")` in the Swift app). The model
/// takes no log crate; the app writes these wherever it logs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UpdateDiagnostic {
    /// How durable it should be.
    pub level: DiagnosticLevel,
    /// The text.
    pub message: String,
}

/// What the model tells the window and the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateEvent {
    /// The state or the log changed: what the window would draw now. One per change, so a
    /// state is never shown only once a later log line happens to redraw the window.
    Changed {
        /// The state as of this change.
        state: UpdateState,
        /// The log as of this change.
        mark: LogMark,
    },
    /// A line for the app's log.
    Diagnostic(UpdateDiagnostic),
}

/// Drives one update: preflight, rebuild of the working tree as it stands, relaunch. Stops
/// at the first failure and keeps the log, so the window can show what happened.
///
/// Open tabs are not this model's concern: the environment hands them over before launching
/// the new bundle. The model only decides when they are handed over and when a failed
/// relaunch takes them back.
pub struct UpdateModel {
    pub(super) build: AppBuild,
    pub(super) environment: Rc<dyn UpdateEnvironment>,
    state: UpdateState,
    log: Vec<String>,
    log_dropped: usize,
    /// Counts attempts, so a line a cancelled build prints late never lands in the next
    /// attempt's log.
    pub(super) attempt: u64,
    /// Whether an attempt is under way, from `start` until the turn that settles its state.
    running: bool,
    /// The running attempt's cancel, signalled by [`Self::cancel`].
    cancel: Option<Arc<CancelToken>>,
    pub(super) inbox: Arc<OutputInbox>,
    _pump: Task<()>,
}

impl EventEmitter<UpdateEvent> for UpdateModel {}

impl UpdateModel {
    /// A model for the running bundle, idle.
    pub fn new(
        build: AppBuild,
        environment: Rc<dyn UpdateEnvironment>,
        cx: &mut Context<Self>,
    ) -> Self {
        let inbox = Arc::new(OutputInbox::default());
        let reader = Arc::clone(&inbox);
        let pump = cx.spawn(async move |this, cx| {
            loop {
                let (attempt, lines) = reader.next().await;
                if this
                    .update(cx, |model, cx| model.output(attempt, lines, cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            build,
            environment,
            state: UpdateState::Idle,
            log: Vec::new(),
            log_dropped: 0,
            attempt: 0,
            running: false,
            cancel: None,
            inbox,
            _pump: pump,
        }
    }

    /// Where the update stands.
    #[must_use]
    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    /// This run's log, at most [`MAX_LOG_LINES`] lines.
    #[must_use]
    pub fn log(&self) -> &[String] {
        &self.log
    }

    /// Head lines trimmed from this run's log at [`MAX_LOG_LINES`].
    #[must_use]
    pub fn log_dropped(&self) -> usize {
        self.log_dropped
    }

    /// Which run the log belongs to: bumped by [`Self::start`], which clears the log.
    #[must_use]
    pub fn log_run(&self) -> u64 {
        self.attempt
    }

    /// Where the log stands now, for a view to keep and pass back to [`Self::log_change`].
    #[must_use]
    pub fn log_mark(&self) -> LogMark {
        LogMark {
            run: self.attempt,
            dropped: self.log_dropped,
            through: self.log_dropped + self.log.len(),
        }
    }

    /// What a view showing the log at `mark` changes to show the log as it is now. `None`
    /// means the view shows nothing yet.
    #[must_use]
    pub fn log_change(&self, mark: Option<LogMark>) -> LogChange {
        let now = self.log_mark();
        let Some(mark) = mark.filter(|mark| mark.run == now.run && mark.through <= now.through)
        else {
            return LogChange::Replace(self.log.clone());
        };
        let fresh = now.through - mark.through;
        if fresh > self.log.len() {
            return LogChange::Replace(self.log.clone());
        }
        let drop_head = now.dropped - mark.dropped;
        if fresh == 0 && drop_head == 0 {
            return LogChange::None;
        }
        LogChange::Append {
            drop_head,
            lines: self.log[self.log.len() - fresh..].to_vec(),
        }
    }

    /// Whether a step is running.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        match self.state {
            UpdateState::Running(_) => true,
            UpdateState::Idle | UpdateState::Installed | UpdateState::Failed { .. } => false,
        }
    }

    /// Whether [`Self::proceed`] would start anything: the bundle can rebuild itself, and the
    /// update is neither running nor already relaunching.
    #[must_use]
    pub fn can_proceed(&self) -> bool {
        can_proceed(&self.build, &self.state)
    }

    /// Runs the whole update; the task resolves once it has settled. Does nothing while an
    /// attempt runs.
    pub fn start(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.is_busy() || self.running {
            return Task::ready(());
        }
        self.attempt += 1;
        self.log.clear();
        self.log_dropped = 0;
        self.running = true;
        let cancel = Arc::new(CancelToken::new());
        self.cancel = Some(Arc::clone(&cancel));
        let (done, finished) = oneshot::channel();
        cx.spawn(async move |this, cx| {
            run::run(&this, cancel, cx).await;
            // The same turn that settled the state, so the model is never idle while still
            // holding a finished attempt.
            let _ = this.update(cx, |model, _| {
                model.running = false;
                model.cancel = None;
            });
            done.send(());
        })
        .detach();
        cx.background_executor().spawn(async move {
            let _ = finished.await;
        })
    }

    /// Stops the attempt under way: the build is killed, and the update ends
    /// failed with "cancelled", so Try Again starts clean. A no-op when nothing runs.
    pub fn cancel(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.signal();
        }
    }

    /// What the window's button does: cancel while the build runs, otherwise
    /// [`Self::proceed`]. The shortcut calls `proceed` alone, so it never cancels.
    pub fn press_action(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if is_cancellable(&self.state) {
            self.cancel();
            return Task::ready(());
        }
        self.proceed(cx)
    }

    /// Returns a terminal state to idle so the window can start a second attempt.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.state = UpdateState::Idle;
        self.changed(cx);
    }

    /// What the window's button and its shortcut do: start from idle, start over from a
    /// failure, and nothing otherwise.
    pub fn proceed(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if !self.can_proceed() {
            return Task::ready(());
        }
        match self.state {
            UpdateState::Failed { .. } => {
                self.reset(cx);
                self.start(cx)
            }
            UpdateState::Idle => self.start(cx),
            UpdateState::Running(_) | UpdateState::Installed => Task::ready(()),
        }
    }

    /// Enters `step`, telling the window and the app's log.
    pub(super) fn enter(&mut self, step: UpdateStep, cx: &mut Context<Self>) {
        Self::diagnose(
            DiagnosticLevel::Notice,
            format!("step: {}", step_name(step)),
            cx,
        );
        self.state = UpdateState::Running(step);
        self.changed(cx);
    }

    /// Settles on a state that is not a failure.
    pub(super) fn settle(&mut self, state: UpdateState, cx: &mut Context<Self>) {
        self.state = state;
        self.changed(cx);
    }

    /// Fails `step`, logging the failure as an error, after the last build lines when the
    /// install failed: the output itself goes out only at `Info`.
    pub(super) fn fail(&mut self, step: UpdateStep, message: &str, cx: &mut Context<Self>) {
        if step == UpdateStep::Install {
            let tail = self.log[self.log.len().saturating_sub(FAILURE_TAIL_LINES)..].join("\n");
            Self::diagnose(
                DiagnosticLevel::Error,
                format!("last build output:\n{tail}"),
                cx,
            );
        }
        let name = step_name(step);
        Self::diagnose(
            DiagnosticLevel::Error,
            format!("{name} failed: {message}"),
            cx,
        );
        self.append(vec![format!("error: {message}")], cx);
        self.state = UpdateState::Failed {
            step,
            message: message.to_owned(),
        };
        self.changed(cx);
    }

    /// A step line: kept in the window's log and in the app's log.
    pub(super) fn note(&mut self, line: String, cx: &mut Context<Self>) {
        Self::diagnose(DiagnosticLevel::Notice, line.clone(), cx);
        self.append(vec![line], cx);
    }

    /// One read of build output from `attempt`: kept in the window's log, and only at `Info`
    /// in the app's log. Dropped when it comes from an earlier attempt.
    pub(super) fn output(&mut self, attempt: u64, lines: Vec<String>, cx: &mut Context<Self>) {
        if attempt != self.attempt {
            return;
        }
        Self::diagnose(DiagnosticLevel::Info, lines.join("\n"), cx);
        self.append(lines, cx);
    }

    pub(super) fn diagnose(level: DiagnosticLevel, message: String, cx: &mut Context<Self>) {
        cx.emit(UpdateEvent::Diagnostic(UpdateDiagnostic { level, message }));
    }

    /// Adds a batch to the window's log: one trim at the cap and one change, however many
    /// lines it holds.
    fn append(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
        if lines.is_empty() {
            return;
        }
        self.log.extend(lines);
        let excess = self.log.len().saturating_sub(MAX_LOG_LINES);
        if excess > 0 {
            self.log.drain(..excess);
            self.log_dropped += excess;
        }
        self.changed(cx);
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.notify();
        cx.emit(UpdateEvent::Changed {
            state: self.state.clone(),
            mark: self.log_mark(),
        });
    }
}

/// The step's name in the app's log, as the Swift app wrote it.
fn step_name(step: UpdateStep) -> &'static str {
    match step {
        UpdateStep::Preflight => "preflight",
        UpdateStep::Install => "install",
        UpdateStep::Relaunch => "relaunch",
    }
}

/// Why a bundle with no source checkout stamp cannot update: it names the script that
/// stamps one.
pub(super) fn unstamped_message() -> String {
    format!(
        "this bundle carries no source checkout; rebuild it with {}",
        AppBuild::BUILD_SCRIPT
    )
}
