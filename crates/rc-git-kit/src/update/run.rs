//! One attempt, step by step. Every step that runs outside the process is noted in the log
//! before it runs, never after: the log's last line is what names a step that stalls.
//!
//! Each function returns `None` when the update must stop; by then the model's state already
//! says why (or the model is gone). Environment calls are made outside any model update, so
//! an environment may read the model while it works.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AsyncApp, Context, WeakEntity};

use crate::guard_bridge::CancelToken;

use super::model::unstamped_message;
use super::{
    OutputSink, SourceState, SourceStateResult, ToolchainCheck, UpdateEnvironment, UpdateMode,
    UpdateModel, UpdateState, UpdateStep, UpdateSyncResult, list,
};

/// How many dirty paths a message names before "and N more".
const LIST_LIMIT: usize = 3;

type Env = Rc<dyn UpdateEnvironment>;

/// Runs `f` on the model; `None` once the model is gone.
fn with<R>(
    this: &WeakEntity<UpdateModel>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut UpdateModel, &mut Context<UpdateModel>) -> R,
) -> Option<R> {
    this.update(cx, f).ok()
}

/// Runs the whole attempt.
pub(super) async fn run(
    this: &WeakEntity<UpdateModel>,
    force: bool,
    cancel: Arc<CancelToken>,
    cx: &mut AsyncApp,
) {
    let _ = steps(this, force, &cancel, cx).await;
}

async fn steps(
    this: &WeakEntity<UpdateModel>,
    force: bool,
    cancel: &Arc<CancelToken>,
    cx: &mut AsyncApp,
) -> Option<()> {
    let (mode, root, env) = with(this, cx, |model, cx| {
        let (Some(mode), Some(root)) = (model.build.update_mode(), model.build.source_root.clone())
        else {
            model.fail(
                UpdateStep::Preflight,
                &unstamped_message(model.build.variant),
                cx,
            );
            return None;
        };
        Some((mode, root, Rc::clone(&model.environment)))
    })??;
    let before = preflight(this, &env, &root, cx).await?;
    match mode {
        UpdateMode::Update => fast_forward(this, &env, (&before, force), cancel, cx).await?,
        UpdateMode::Rebuild => with(this, cx, |model, cx| note_rebuild(model, &before, cx))?,
    }
    build_and_relaunch(this, &env, mode, cancel, cx).await
}

/// Checks the toolchain and reads the checkout. Both halves are logged before they run.
async fn preflight(
    this: &WeakEntity<UpdateModel>,
    env: &Env,
    root: &str,
    cx: &mut AsyncApp,
) -> Option<SourceState> {
    with(this, cx, |model, cx| {
        model.enter(UpdateStep::Preflight, cx);
        model.note(format!("checkout: {root}"), cx);
        model.note("checking the toolchain".to_owned(), cx);
    })?;
    let failure = match cx.update(|cx| env.toolchain(cx)).await {
        ToolchainCheck::Ready => None,
        ToolchainCheck::Missing(tools) => Some(format!(
            "your login shell cannot find {}; add the toolchain to ~/.zshenv or ~/.zprofile \
             and try again",
            tools.join(", ")
        )),
        ToolchainCheck::Failure(message) => Some(message),
    };
    if let Some(message) = failure {
        with(this, cx, |model, cx| {
            model.fail(UpdateStep::Preflight, &message, cx);
        });
        return None;
    }
    with(this, cx, |model, cx| {
        model.note("reading the checkout".to_owned(), cx);
    })?;
    let state = read_source(this, env, UpdateStep::Preflight, cx).await?;
    let branch = state.branch.as_deref().unwrap_or("a detached HEAD");
    with(this, cx, |model, cx| model.note(format!("on {branch}"), cx))?;
    Some(state)
}

/// The dev bundle's half: no fetch and no dirty check, only a note of what goes in.
fn note_rebuild(model: &mut UpdateModel, before: &SourceState, cx: &mut Context<UpdateModel>) {
    if !before.dirty_paths.is_empty() {
        let paths = list(&before.dirty_paths, LIST_LIMIT);
        model.note(format!("uncommitted work goes in: {paths}"), cx);
    }
    if !before.scan_complete {
        model.note(
            "the status scan stopped at its cap, so more uncommitted work may go in than is \
             listed"
                .to_owned(),
            cx,
        );
    }
}

/// The release half: refuse uncommitted work, fetch and fast-forward, then compare the result
/// with the commit this bundle was built from. `None` when a step failed or there is nothing
/// new to build.
async fn fast_forward(
    this: &WeakEntity<UpdateModel>,
    env: &Env,
    (before, force): (&SourceState, bool),
    cancel: &Arc<CancelToken>,
    cx: &mut AsyncApp,
) -> Option<()> {
    if !before.is_clean() {
        with(this, cx, |model, cx| {
            model.fail(UpdateStep::Preflight, &unclean_message(before), cx);
        });
        return None;
    }
    with(this, cx, |model, cx| {
        model.enter(UpdateStep::Sync, cx);
        model.note("fetching and fast-forwarding".to_owned(), cx);
    })?;
    let result = cx.update(|cx| env.sync(Arc::clone(cancel), cx)).await;
    unless_cancelled(this, cancel, UpdateStep::Sync, cx)?;
    let notice = match result {
        UpdateSyncResult::Failure(message) => {
            with(this, cx, |model, cx| {
                model.fail(UpdateStep::Sync, &message, cx);
            });
            return None;
        }
        UpdateSyncResult::Synced { notice } => notice,
    };
    with(this, cx, |model, cx| {
        if let Some(notice) = notice {
            model.note(format!("warning: {notice}"), cx);
        }
        model.note("reading the checkout after the fetch".to_owned(), cx);
    })?;
    let after = read_source(this, env, UpdateStep::Sync, cx).await?;
    let up_to_date = with(this, cx, |model, cx| {
        let short = after.commit.get(..7).unwrap_or(&after.commit);
        model.note(format!("checkout is at {short}"), cx);
        let unchanged = model.build.commit.as_deref() == Some(after.commit.as_str());
        !force && !model.build.dirty && unchanged
    })?;
    if up_to_date {
        with(this, cx, |model, cx| {
            UpdateModel::diagnose(
                super::DiagnosticLevel::Notice,
                "checkout is already at this build; nothing to install".to_owned(),
                cx,
            );
            model.settle(UpdateState::UpToDate, cx);
        });
        return None;
    }
    Some(())
}

/// Builds the bundle, puts it in place, hands the tabs over, launches it, and quits.
async fn build_and_relaunch(
    this: &WeakEntity<UpdateModel>,
    env: &Env,
    mode: UpdateMode,
    cancel: &Arc<CancelToken>,
    cx: &mut AsyncApp,
) -> Option<()> {
    let (attempt, inbox) = with(this, cx, |model, cx| {
        model.enter(UpdateStep::Install, cx);
        let line = match mode {
            UpdateMode::Update => "building",
            UpdateMode::Rebuild => "rebuilding the working tree",
        };
        model.note(line.to_owned(), cx);
        (model.attempt, Arc::clone(&model.inbox))
    })?;
    let sink: OutputSink = Arc::new(move |lines| inbox.push(attempt, lines));
    let message = cx
        .update(|cx| env.install(Arc::clone(cancel), sink, cx))
        .await;
    with(this, cx, |model, cx| {
        for (from, lines) in model.inbox.take_all() {
            model.output(from, lines, cx);
        }
    })?;
    unless_cancelled(this, cancel, UpdateStep::Install, cx)?;
    if let Some(message) = message {
        with(this, cx, |model, cx| {
            model.fail(UpdateStep::Install, &message, cx);
        });
        return None;
    }
    relaunch(this, env, cx).await
}

async fn relaunch(this: &WeakEntity<UpdateModel>, env: &Env, cx: &mut AsyncApp) -> Option<()> {
    with(this, cx, |model, cx| {
        model.enter(UpdateStep::Relaunch, cx);
        model.note("launching the new build".to_owned(), cx);
    })?;
    cx.update(|cx| env.hand_off(cx));
    if let Some(message) = cx.update(|cx| env.relaunch(cx)).await {
        cx.update(|cx| env.reclaim(cx));
        with(this, cx, |model, cx| {
            model.fail(UpdateStep::Relaunch, &message, cx);
        });
        return None;
    }
    with(this, cx, |model, cx| {
        model.settle(UpdateState::Installed, cx);
        model.note("quitting this instance".to_owned(), cx);
    })?;
    cx.update(|cx| env.quit(cx));
    Some(())
}

async fn read_source(
    this: &WeakEntity<UpdateModel>,
    env: &Env,
    step: UpdateStep,
    cx: &mut AsyncApp,
) -> Option<SourceState> {
    match cx.update(|cx| env.source_state(cx)).await {
        SourceStateResult::State(state) => Some(state),
        SourceStateResult::Failure(message) => {
            with(this, cx, |model, cx| model.fail(step, &message, cx));
            None
        }
    }
}

/// Ends the update as cancelled when the cancel landed during `step`. Checked after the step
/// returns, so a build that finished at the moment of the cancel still never relaunches.
fn unless_cancelled(
    this: &WeakEntity<UpdateModel>,
    cancel: &CancelToken,
    step: UpdateStep,
    cx: &mut AsyncApp,
) -> Option<()> {
    if !cancel.is_signalled() {
        return Some(());
    }
    with(this, cx, |model, cx| model.fail(step, "cancelled", cx));
    None
}

/// Why a checkout that is not clean stops the update: the paths when there are any,
/// otherwise the scan that could not finish.
fn unclean_message(state: &SourceState) -> String {
    if !state.dirty_paths.is_empty() {
        let paths = list(&state.dirty_paths, LIST_LIMIT);
        return format!("the checkout has uncommitted work: {paths}");
    }
    "the status scan stopped at its cap before covering the checkout, so uncommitted work \
     cannot be ruled out; run scripts/install-app.sh in a terminal"
        .to_owned()
}
