//! The workflow plumbing: one [`Flow`] per running workflow carries the handles a step needs
//! off the model, so each step reads and writes the model only between awaits. Also the
//! coalesced status read.

use std::sync::Arc;

use gpui::{AsyncApp, Context, Task, WeakEntity};
use rc_git_core::DefaultBranch;
use rc_git_guard::actions::{
    ActionError, CheckoutOutcome, FetchOutcome, PullOutcome, PushOutcome, UpstreamStatus,
};

use super::SyncModel;
use super::values::{SyncStep, SyncSummary, checkout_warning, no_upstream_message, push_warning};
use crate::blocking::blocking;
use crate::guard_bridge::{CancelToken, FailureKind, classify};
use crate::models::branch_refusal;
use crate::platform::credentials::CredentialStore;
use crate::platform::host_keys::HostKeyStore;
use crate::repository_actions::RepositoryActions;

/// The workflows the toolbar and menu start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Workflow {
    Fetch,
    Pull,
    Push,
    Sync,
    BackToDefault,
}

impl Workflow {
    /// The step the workflow shows first.
    pub(super) fn start(self) -> SyncStep {
        match self {
            Self::Fetch | Self::Pull | Self::Sync => SyncStep::Fetch,
            Self::Push => SyncStep::Push,
            Self::BackToDefault => SyncStep::Checkout,
        }
    }
}

/// A step that failed in a way the workflow reports and stops on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StepFailure {
    pub(super) step: SyncStep,
    pub(super) message: String,
    pub(super) diverged: bool,
}

impl StepFailure {
    pub(super) fn halt(step: SyncStep, message: impl Into<String>) -> Halt {
        Halt::Failed(Self {
            step,
            message: message.into(),
            diverged: false,
        })
    }

    /// The failure `error` makes of `step`, diverged when the histories split.
    pub(super) fn of(step: SyncStep, error: &ActionError) -> Halt {
        Halt::Failed(Self {
            step,
            message: error.to_string(),
            diverged: classify(error) == FailureKind::Diverged,
        })
    }
}

/// Why a workflow stopped before its end: a step failed, or the model was released and
/// nobody is left to tell.
#[derive(Debug)]
pub(super) enum Halt {
    Failed(StepFailure),
    Released,
}

/// What a step hands on.
pub(super) type Flowing<T> = Result<T, Halt>;

/// What a finished workflow hands the model.
pub(super) type Outcome = Result<SyncSummary, StepFailure>;

/// One running workflow's handles.
pub(super) struct Flow {
    pub(super) this: WeakEntity<SyncModel>,
    pub(super) generation: u64,
    pub(super) cancel: Arc<CancelToken>,
    pub(super) actions: Arc<dyn RepositoryActions>,
    pub(super) store: Arc<dyn CredentialStore>,
    pub(super) host_keys: Arc<dyn HostKeyStore>,
}

impl Flow {
    pub(super) fn new(model: &SyncModel, cx: &Context<SyncModel>) -> Self {
        Self {
            this: cx.weak_entity(),
            generation: model.generation,
            cancel: Arc::clone(&model.cancel),
            actions: Arc::clone(&model.actions),
            store: Arc::clone(&model.store),
            host_keys: Arc::clone(&model.host_keys),
        }
    }

    /// Runs `update` on the model, or halts when it is gone.
    pub(super) fn model<R>(
        &self,
        cx: &mut AsyncApp,
        update: impl FnOnce(&mut SyncModel, &mut Context<SyncModel>) -> R,
    ) -> Flowing<R> {
        self.this.update(cx, update).map_err(|_| Halt::Released)
    }

    pub(super) fn enter(&self, step: SyncStep, cx: &mut AsyncApp) -> Flowing<()> {
        self.model(cx, |model, cx| model.enter(step, cx))
    }

    /// Stops the workflow when the user cancelled: a cancelled transfer can surface as an
    /// auth or host-key failure, and neither should put a sheet on screen after the user has
    /// already said stop.
    pub(super) fn bail_if_cancelled(&self, step: SyncStep) -> Flowing<()> {
        if self.cancel.is_signalled() {
            return Err(StepFailure::halt(step, "cancelled"));
        }
        Ok(())
    }

    /// Runs `work` against the actions on its own thread.
    pub(super) async fn run_blocking<T, F>(&self, cx: &mut AsyncApp, work: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(&dyn RepositoryActions) -> T + Send + 'static,
    {
        let actions = Arc::clone(&self.actions);
        blocking(cx.background_executor(), move || work(actions.as_ref())).await
    }

    /// Runs `workflow` to its summary, or to the failure that stopped it. With an upstream
    /// check, the upstream is re-read first and the workflow refuses on that fresh answer,
    /// never on the cached one: a `git remote add` or `git checkout --detach` in a terminal
    /// since the last refresh decides, and the network steps key credentials and host keys on
    /// the URL the guard will dial.
    pub(super) async fn run(&self, workflow: Workflow, cx: &mut AsyncApp) -> Flowing<SyncSummary> {
        if workflow != Workflow::BackToDefault {
            self.adopt_fresh_upstream(workflow.start(), cx).await?;
        }
        match workflow {
            Workflow::Fetch => {
                let fetched = self.fetch(cx).await?;
                Ok(SyncSummary {
                    fetched_refs: fetched.updated_refs.len(),
                    ..SyncSummary::default()
                })
            }
            Workflow::Pull => {
                let fetched = self.fetch(cx).await?;
                let pulled = self.pull(cx).await?;
                Ok(SyncSummary {
                    fetched_refs: fetched.updated_refs.len(),
                    pulled_files: pulled.files_changed,
                    ..SyncSummary::default()
                })
            }
            Workflow::Push => {
                let pushed = self.push(cx).await?;
                Ok(SyncSummary {
                    pushed_commits: pushed.pushed_commits,
                    notice: push_warning(&pushed),
                    ..SyncSummary::default()
                })
            }
            Workflow::Sync => self.sync(cx).await,
            Workflow::BackToDefault => self.back_to_default(cx).await,
        }
    }

    /// Fetch, fast-forward, then push. A cancel that landed during the pull, which cannot
    /// stop, keeps the pull and skips the push.
    async fn sync(&self, cx: &mut AsyncApp) -> Flowing<SyncSummary> {
        let fetched = self.fetch(cx).await?;
        let pulled = self.pull(cx).await?;
        if self.cancel.is_signalled() {
            return Ok(SyncSummary {
                fetched_refs: fetched.updated_refs.len(),
                pulled_files: pulled.files_changed,
                cancelled_after: Some(SyncStep::Pull),
                ..SyncSummary::default()
            });
        }
        let pushed = self.push(cx).await?;
        Ok(SyncSummary {
            fetched_refs: fetched.updated_refs.len(),
            pulled_files: pulled.files_changed,
            pushed_commits: pushed.pushed_commits,
            cancelled_after: None,
            notice: push_warning(&pushed),
        })
    }

    /// Switch to the mainline branch, then fetch and fast-forward it: one control for "give
    /// me the latest master to work on".
    async fn back_to_default(&self, cx: &mut AsyncApp) -> Flowing<SyncSummary> {
        let branch = self.resolve_default_branch(cx).await?;
        let switched = self.checkout(branch, cx).await?;
        if !switched.already_on_branch {
            let name = switched.branch.clone();
            self.model(cx, |model, _| model.switched_branch = Some(name))?;
        }
        let warning = checkout_warning(&switched);
        if self.cancel.is_signalled() {
            return Ok(SyncSummary {
                pulled_files: switched.files_changed,
                cancelled_after: Some(SyncStep::Checkout),
                notice: warning,
                ..SyncSummary::default()
            });
        }
        self.adopt_fresh_upstream(SyncStep::Fetch, cx).await?;
        let fetched = self.fetch(cx).await?;
        let pulled = self.pull(cx).await?;
        Ok(SyncSummary {
            fetched_refs: fetched.updated_refs.len(),
            pulled_files: pulled.files_changed + switched.files_changed,
            pushed_commits: 0,
            cancelled_after: None,
            notice: warning,
        })
    }

    /// One status read off the UI thread, its failure as a message ready to show.
    pub(super) async fn read_status(&self, cx: &mut AsyncApp) -> Result<UpstreamStatus, String> {
        self.run_blocking(cx, |actions| actions.upstream_status())
            .await
            .map_err(|error| error.to_string())
    }

    /// Re-reads the upstream and makes it the status everything else reads, so the steps
    /// that follow authenticate against the remote the guard will dial. A failed read or a
    /// branch with no upstream stops the workflow here rather than opening a connection it
    /// cannot name. A refresh started before this read drops its older answer.
    async fn adopt_fresh_upstream(&self, step: SyncStep, cx: &mut AsyncApp) -> Flowing<()> {
        self.model(cx, |model, _| model.status.generation += 1)?;
        let status = self.read_status(cx).await;
        let applied = status.clone();
        self.model(cx, |model, cx| model.apply_status(applied, cx))?;
        match status {
            Err(message) => Err(StepFailure::halt(
                step,
                format!("cannot read the upstream status: {message}"),
            )),
            Ok(status) if !status.has_upstream => {
                Err(StepFailure::halt(step, no_upstream_message(&status.branch)))
            }
            Ok(_) => Ok(()),
        }
    }

    /// The branch Back to default targets. Fails the workflow when nothing names one, with
    /// the cause when the repository could not be read, and when the name is not valid
    /// UTF-8, because its repaired spelling would check out a different branch.
    async fn resolve_default_branch(&self, cx: &mut AsyncApp) -> Flowing<String> {
        self.bail_if_cancelled(SyncStep::Checkout)?;
        self.enter(SyncStep::Checkout, cx)?;
        if let Some(branch) = self.model(cx, |model, _| model.mainline.clone())? {
            return checkout_name(branch);
        }
        let read = self
            .run_blocking(cx, |actions| actions.default_branch())
            .await;
        let found = match read {
            Ok(found) => found,
            Err(error) => {
                return Err(StepFailure::halt(
                    SyncStep::Checkout,
                    format!("cannot read the mainline branch: {error}"),
                ));
            }
        };
        let remembered = found.clone();
        self.model(cx, |model, _| {
            model.mainline = remembered;
            model.default_branch_read = true;
        })?;
        let Some(branch) = found else {
            return Err(StepFailure::halt(
                SyncStep::Checkout,
                "cannot tell which branch is the mainline: no remote HEAD, and no local \
                 'master' or 'main'",
            ));
        };
        checkout_name(branch)
    }

    async fn checkout(&self, branch: String, cx: &mut AsyncApp) -> Flowing<CheckoutOutcome> {
        self.bail_if_cancelled(SyncStep::Checkout)?;
        self.enter(SyncStep::Checkout, cx)?;
        self.run_blocking(cx, move |actions| actions.checkout(&branch))
            .await
            .map_err(|error| StepFailure::of(SyncStep::Checkout, &error))
    }

    async fn pull(&self, cx: &mut AsyncApp) -> Flowing<PullOutcome> {
        self.bail_if_cancelled(SyncStep::Pull)?;
        self.enter(SyncStep::Pull, cx)?;
        self.run_blocking(cx, |actions| actions.pull_fast_forward())
            .await
            .map_err(|error| StepFailure::of(SyncStep::Pull, &error))
    }

    async fn fetch(&self, cx: &mut AsyncApp) -> Flowing<FetchOutcome> {
        self.network_step(
            SyncStep::Fetch,
            |actions, remote, cancel| actions.fetch(remote, cancel),
            cx,
        )
        .await
    }

    async fn push(&self, cx: &mut AsyncApp) -> Flowing<PushOutcome> {
        self.network_step(
            SyncStep::Push,
            |actions, remote, cancel| actions.push(remote, cancel),
            cx,
        )
        .await
    }
}

/// The name to check out for `branch`, refused when it was repaired on its way out of the
/// core.
fn checkout_name(branch: DefaultBranch) -> Flowing<String> {
    if !branch.exact_name {
        return Err(StepFailure::halt(
            SyncStep::Checkout,
            branch_refusal("switch to", &branch.name),
        ));
    }
    Ok(branch.name)
}

/// One status read with, the first time, the mainline branch.
struct StatusRead {
    generation: u64,
    status: Task<Result<UpstreamStatus, ActionError>>,
    branch: Option<Task<Result<Option<DefaultBranch>, ActionError>>>,
}

impl SyncModel {
    /// Re-reads the upstream status. Cheap and local; skipped while a workflow runs. The
    /// mainline branch comes along until it has been read once.
    ///
    /// Coalesces: a call made while a read runs marks one more read wanted and returns an
    /// already finished task, and the running loop reads once more when it lands.
    pub fn refresh_status(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.status.pending = true;
        if self.status.running {
            return Task::ready(());
        }
        self.status.running = true;
        let first = self.begin_status_read(cx);
        cx.spawn(async move |this, cx| {
            let mut next = first;
            while let Some(read) = next {
                let status = read.status.await;
                let branch = match read.branch {
                    Some(branch) => Some(branch.await),
                    None => None,
                };
                let Ok(following) = this.update(cx, |model, cx| {
                    model.land_status_read(read.generation, status, branch, cx);
                    model.begin_status_read(cx)
                }) else {
                    return;
                };
                next = following;
            }
        })
    }

    /// Starts the next read when one is pending and no workflow runs, else ends the loop.
    fn begin_status_read(&mut self, cx: &mut Context<Self>) -> Option<StatusRead> {
        let wanted = std::mem::take(&mut self.status.pending);
        if !wanted || self.state.is_busy() {
            self.status.running = false;
            return None;
        }
        self.status.generation += 1;
        let actions = Arc::clone(&self.actions);
        let status = blocking(cx.background_executor(), move || actions.upstream_status());
        let branch = (!self.default_branch_read).then(|| {
            let actions = Arc::clone(&self.actions);
            blocking(cx.background_executor(), move || actions.default_branch())
        });
        Some(StatusRead {
            generation: self.status.generation,
            status,
            branch,
        })
    }

    fn land_status_read(
        &mut self,
        generation: u64,
        status: Result<UpstreamStatus, ActionError>,
        branch: Option<Result<Option<DefaultBranch>, ActionError>>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.status.generation || self.state.is_busy() {
            return;
        }
        if let Some(Ok(branch)) = branch {
            self.mainline = branch;
            self.default_branch_read = true;
        }
        self.apply_status(status.map_err(|error| error.to_string()), cx);
    }
}
