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
    OutputSink, SourceState, SourceStateResult, ToolchainCheck, UpdateEnvironment, UpdateModel,
    UpdateState, UpdateStep, list,
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
    cancel: Arc<CancelToken>,
    cx: &mut AsyncApp,
) {
    let _ = steps(this, &cancel, cx).await;
}

async fn steps(
    this: &WeakEntity<UpdateModel>,
    cancel: &Arc<CancelToken>,
    cx: &mut AsyncApp,
) -> Option<()> {
    let (root, env) = with(this, cx, |model, cx| {
        let Some(root) = model.build.source_root.clone() else {
            model.fail(UpdateStep::Preflight, &unstamped_message(), cx);
            return None;
        };
        Some((root, Rc::clone(&model.environment)))
    })??;
    let before = preflight(this, &env, &root, cx).await?;
    with(this, cx, |model, cx| note_rebuild(model, &before, cx))?;
    build_and_relaunch(this, &env, cancel, cx).await
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

/// No fetch and no dirty check, only a note of what goes in.
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

/// Rebuilds the bundle, hands the tabs over, launches it, and quits.
async fn build_and_relaunch(
    this: &WeakEntity<UpdateModel>,
    env: &Env,
    cancel: &Arc<CancelToken>,
    cx: &mut AsyncApp,
) -> Option<()> {
    let (attempt, inbox) = with(this, cx, |model, cx| {
        model.enter(UpdateStep::Install, cx);
        model.note("rebuilding the working tree".to_owned(), cx);
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
