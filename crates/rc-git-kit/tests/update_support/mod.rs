//! Environments for the update model's tests, ported from `UpdateModelTests.swift`: a
//! scripted stub that records every call, a probe held at a gate, and a fetch or build that
//! never returns on its own.

#![expect(
    dead_code,
    reason = "each test file uses a different part of the support"
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use gpui::{App, AppContext as _, Entity, Task, TestAppContext, WeakEntity};
use rc_git_kit::app_build::{AppBuild, AppVariant};
use rc_git_kit::blocking::blocking;
use rc_git_kit::guard_bridge::CancelToken;
use rc_git_kit::oneshot;
use rc_git_kit::update::{
    LogMark, OutputSink, SourceState, SourceStateResult, ToolchainCheck, UpdateEnvironment,
    UpdateEvent, UpdateModel, UpdateState, UpdateStep, UpdateSyncResult,
};

pub const HEAD: &str = "a3f91c2d4e5f60718293a4b5c6d7e8f901234567";
pub const UPSTREAM: &str = "b7d21ff0112233445566778899aabbccddeeff00";

/// A bundle stamped like `scripts/build-app.sh` stamps one.
pub fn build(variant: AppVariant, source_root: Option<&str>, commit: Option<&str>) -> AppBuild {
    AppBuild {
        variant,
        name: "rc-git".to_owned(),
        version: "0.1.0".to_owned(),
        build: "42".to_owned(),
        source_root: source_root.map(str::to_owned),
        commit: commit.map(str::to_owned),
        dirty: false,
        build_date: None,
    }
}

/// The release bundle built clean at [`HEAD`] from `/checkout`.
pub fn release() -> AppBuild {
    build(AppVariant::Release, Some("/checkout"), Some(HEAD))
}

/// A clean checkout at `commit` on master.
pub fn clean(commit: &str) -> SourceStateResult {
    SourceStateResult::State(SourceState::new(commit, Some("master"), &[]))
}

/// What the stub answers.
#[derive(Debug, Clone)]
pub struct Script {
    pub toolchain: ToolchainCheck,
    /// One entry per `source_state` call: preflight reads it, then the sync step does.
    pub source_states: Vec<SourceStateResult>,
    pub sync_failure: Option<String>,
    /// A problem the pull survived, reported with a successful sync.
    pub sync_notice: Option<String>,
    pub install_failure: Option<String>,
    /// Printed by the build in one read.
    pub install_output: Vec<String>,
    /// Printed by the build after `install_output`, one read per batch.
    pub install_batches: Vec<Vec<String>>,
    pub relaunch_failure: Option<String>,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            toolchain: ToolchainCheck::Ready,
            source_states: Vec::new(),
            sync_failure: None,
            sync_notice: None,
            install_failure: None,
            install_output: Vec::new(),
            install_batches: Vec::new(),
            relaunch_failure: None,
        }
    }
}

impl Script {
    /// A release update whose fetch moves the checkout from [`HEAD`] to [`UPSTREAM`].
    pub fn synced() -> Self {
        Self {
            source_states: vec![clean(HEAD), clean(UPSTREAM)],
            ..Self::default()
        }
    }
}

/// A scripted environment: it records what the model asked for and answers from the
/// script, so the whole update runs without a toolchain, a network, or a build.
#[derive(Default)]
pub struct Stub {
    script: RefCell<Script>,
    calls: RefCell<Vec<&'static str>>,
    model: RefCell<Option<WeakEntity<UpdateModel>>>,
    /// Set, each step records the model's last log line as it started, so a test can tell a
    /// note logged before its step from one logged after.
    pub probe_last_line: Cell<bool>,
    last_lines: RefCell<Vec<Option<String>>>,
    state_at_quit: RefCell<Option<UpdateState>>,
}

impl Stub {
    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }

    pub fn last_lines(&self) -> Vec<Option<String>> {
        self.last_lines.borrow().clone()
    }

    /// What the window showed as the model quit.
    pub fn state_at_quit(&self) -> Option<UpdateState> {
        self.state_at_quit.borrow().clone()
    }

    fn call(&self, name: &'static str, cx: &App) {
        if self.probe_last_line.get() {
            let line = self.read(cx, |model| model.log().last().cloned()).flatten();
            self.last_lines.borrow_mut().push(line);
        }
        self.calls.borrow_mut().push(name);
    }

    fn read<R>(&self, cx: &App, f: impl FnOnce(&UpdateModel) -> R) -> Option<R> {
        let model = self.model.borrow().as_ref()?.upgrade()?;
        Some(f(model.read(cx)))
    }
}

impl UpdateEnvironment for Stub {
    fn toolchain(&self, cx: &mut App) -> Task<ToolchainCheck> {
        self.call("toolchain", cx);
        Task::ready(self.script.borrow().toolchain.clone())
    }

    /// Answers each call with the next scripted state, repeating the last one.
    fn source_state(&self, cx: &mut App) -> Task<SourceStateResult> {
        self.call("sourceState", cx);
        let mut script = self.script.borrow_mut();
        let answer = match script.source_states.len() {
            0 => SourceStateResult::Failure("no source state scripted".to_owned()),
            1 => script.source_states[0].clone(),
            _ => script.source_states.remove(0),
        };
        Task::ready(answer)
    }

    fn sync(&self, _cancel: Arc<CancelToken>, cx: &mut App) -> Task<UpdateSyncResult> {
        self.call("sync", cx);
        let script = self.script.borrow();
        Task::ready(match &script.sync_failure {
            Some(failure) => UpdateSyncResult::Failure(failure.clone()),
            None => UpdateSyncResult::Synced {
                notice: script.sync_notice.clone(),
            },
        })
    }

    fn install(
        &self,
        _cancel: Arc<CancelToken>,
        on_output: OutputSink,
        cx: &mut App,
    ) -> Task<Option<String>> {
        self.call("install", cx);
        let script = self.script.borrow();
        if !script.install_output.is_empty() {
            on_output(script.install_output.clone());
        }
        for batch in &script.install_batches {
            on_output(batch.clone());
        }
        Task::ready(script.install_failure.clone())
    }

    fn hand_off(&self, cx: &mut App) {
        self.call("handOff", cx);
    }

    fn reclaim(&self, cx: &mut App) {
        self.call("reclaim", cx);
    }

    fn relaunch(&self, cx: &mut App) -> Task<Option<String>> {
        self.call("relaunch", cx);
        Task::ready(self.script.borrow().relaunch_failure.clone())
    }

    fn quit(&self, cx: &mut App) {
        self.call("quit", cx);
        let state = self.read(cx, |model| model.state().clone());
        *self.state_at_quit.borrow_mut() = state;
    }
}

/// A model over a stub answering `script`.
pub fn model(
    cx: &mut TestAppContext,
    script: Script,
    build: AppBuild,
) -> (Entity<UpdateModel>, Rc<Stub>) {
    let stub = Rc::new(Stub {
        script: RefCell::new(script),
        ..Stub::default()
    });
    let environment: Rc<dyn UpdateEnvironment> = Rc::clone(&stub) as Rc<dyn UpdateEnvironment>;
    let model = cx.new(|cx| UpdateModel::new(build, environment, cx));
    *stub.model.borrow_mut() = Some(model.downgrade());
    (model, stub)
}

/// Starts an update and waits for it to settle.
pub async fn start(model: &Entity<UpdateModel>, force: bool, cx: &mut TestAppContext) {
    model.update(cx, |model, cx| model.start(force, cx)).await;
    cx.run_until_parked();
}

/// What the window saw at each change: the state and how far the log reached.
pub type Frame = (UpdateState, LogMark);

/// Every event the model emits, in order.
pub fn events(
    model: &Entity<UpdateModel>,
    cx: &mut TestAppContext,
) -> Rc<RefCell<Vec<UpdateEvent>>> {
    let seen: Rc<RefCell<Vec<UpdateEvent>>> = Rc::default();
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(model, move |_, event, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    seen
}

/// The `Changed` events among `events`.
pub fn frames(events: &[UpdateEvent]) -> Vec<Frame> {
    let mut frames = Vec::new();
    for event in events {
        if let UpdateEvent::Changed { state, mark } = event {
            frames.push((state.clone(), *mark));
        }
    }
    frames
}

/// Whether every redraw carried one change, so no state was shown only once a later log
/// line happened to redraw the window. Starts from the fresh run, idle with no log.
pub fn each_redraw_carries_one_change(frames: &[Frame]) -> bool {
    let Some(first) = frames.first() else {
        return true;
    };
    let start = LogMark {
        run: first.1.run,
        dropped: 0,
        through: 0,
    };
    let mut previous = (UpdateState::Idle, start);
    for frame in frames {
        if previous.0 != frame.0 && previous.1 != frame.1 {
            return false;
        }
        previous = frame.clone();
    }
    true
}

/// Holds the first toolchain probe until the test releases it, so a test can act while the
/// model sits in preflight. A later probe answers at once, so a second attempt the model
/// failed to refuse shows up as a failure instead of a hang. Every other step fails fast.
pub struct GatedToolchain {
    held: RefCell<Option<oneshot::Receiver<()>>>,
    calls: Cell<usize>,
}

impl GatedToolchain {
    /// The environment and the sender that releases its probe.
    pub fn new() -> (Rc<Self>, oneshot::Sender<()>) {
        let (release, held) = oneshot::channel();
        let environment = Rc::new(Self {
            held: RefCell::new(Some(held)),
            calls: Cell::new(0),
        });
        (environment, release)
    }

    pub fn toolchain_calls(&self) -> usize {
        self.calls.get()
    }
}

impl UpdateEnvironment for GatedToolchain {
    fn toolchain(&self, cx: &mut App) -> Task<ToolchainCheck> {
        self.calls.set(self.calls.get() + 1);
        let Some(held) = self.held.borrow_mut().take() else {
            return Task::ready(ToolchainCheck::Failure("a second probe ran".to_owned()));
        };
        cx.foreground_executor().spawn(async move {
            let _ = held.await;
            ToolchainCheck::Failure("released".to_owned())
        })
    }

    fn source_state(&self, _cx: &mut App) -> Task<SourceStateResult> {
        Task::ready(SourceStateResult::Failure("unused".to_owned()))
    }

    fn sync(&self, _cancel: Arc<CancelToken>, _cx: &mut App) -> Task<UpdateSyncResult> {
        Task::ready(UpdateSyncResult::Failure("unused".to_owned()))
    }

    fn install(
        &self,
        _cancel: Arc<CancelToken>,
        _on_output: OutputSink,
        _cx: &mut App,
    ) -> Task<Option<String>> {
        Task::ready(Some("unused".to_owned()))
    }

    fn hand_off(&self, _cx: &mut App) {}

    fn reclaim(&self, _cx: &mut App) {}

    fn relaunch(&self, _cx: &mut App) -> Task<Option<String>> {
        Task::ready(Some("unused".to_owned()))
    }

    fn quit(&self, _cx: &mut App) {}
}

/// A fetch or a build that never returns on its own: the first call to the `hanging` step
/// waits on a thread until its cancel is signalled, like a build waiting on another build's
/// lock. Every later call succeeds at once, so a second attempt runs clean.
pub struct HangingStep {
    hanging: UpdateStep,
    calls: RefCell<Vec<&'static str>>,
    /// The first build's output sink, kept so a test can print a line late.
    first_output: RefCell<Option<OutputSink>>,
}

impl HangingStep {
    pub fn new(hanging: UpdateStep) -> Rc<Self> {
        Rc::new(Self {
            hanging,
            calls: RefCell::default(),
            first_output: RefCell::default(),
        })
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }

    pub fn count(&self, name: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|call| **call == name)
            .count()
    }

    pub fn first_output(&self) -> Option<OutputSink> {
        self.first_output.borrow().clone()
    }

    /// Waits on a thread until `cancel` is signalled when `step` is the hanging one and this
    /// is its first call; answers `answer(hung)`.
    fn hang_once<T: Send + 'static>(
        &self,
        step: UpdateStep,
        cancel: Arc<CancelToken>,
        answer: impl FnOnce(bool) -> T + Send + 'static,
        cx: &mut App,
    ) -> Task<T> {
        let name = if step == UpdateStep::Sync {
            "sync"
        } else {
            "install"
        };
        self.calls.borrow_mut().push(name);
        if step != self.hanging || self.count(name) != 1 {
            return Task::ready(answer(false));
        }
        blocking(cx.background_executor(), move || {
            while !cancel.is_signalled() {
                thread::sleep(Duration::from_millis(2));
            }
            answer(true)
        })
    }
}

impl UpdateEnvironment for HangingStep {
    fn toolchain(&self, _cx: &mut App) -> Task<ToolchainCheck> {
        self.calls.borrow_mut().push("toolchain");
        Task::ready(ToolchainCheck::Ready)
    }

    fn source_state(&self, _cx: &mut App) -> Task<SourceStateResult> {
        self.calls.borrow_mut().push("sourceState");
        let commit = if self.count("sourceState") == 1 {
            HEAD
        } else {
            UPSTREAM
        };
        Task::ready(clean(commit))
    }

    fn sync(&self, cancel: Arc<CancelToken>, cx: &mut App) -> Task<UpdateSyncResult> {
        let answer = |hung| {
            if hung {
                UpdateSyncResult::Failure("the pull was cancelled".to_owned())
            } else {
                UpdateSyncResult::Synced { notice: None }
            }
        };
        self.hang_once(UpdateStep::Sync, cancel, answer, cx)
    }

    /// Reports success even when cancelled, like a build that finished at the moment of the
    /// cancel: the model alone must keep it from relaunching.
    fn install(
        &self,
        cancel: Arc<CancelToken>,
        on_output: OutputSink,
        cx: &mut App,
    ) -> Task<Option<String>> {
        self.first_output.borrow_mut().get_or_insert(on_output);
        self.hang_once(UpdateStep::Install, cancel, |_| None, cx)
    }

    fn hand_off(&self, _cx: &mut App) {}

    fn reclaim(&self, _cx: &mut App) {}

    fn relaunch(&self, _cx: &mut App) -> Task<Option<String>> {
        self.calls.borrow_mut().push("relaunch");
        Task::ready(None)
    }

    fn quit(&self, _cx: &mut App) {
        self.calls.borrow_mut().push("quit");
    }
}

/// A model over `environment` for the release bundle.
pub fn model_over(
    cx: &mut TestAppContext,
    environment: Rc<dyn UpdateEnvironment>,
) -> Entity<UpdateModel> {
    cx.new(|cx| UpdateModel::new(release(), environment, cx))
}
