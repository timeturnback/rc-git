//! One guarded GitHub action of a tab as a state machine free of GPUI ([`Flow`]): planning (the
//! guard reads the pull request again and the policy words the challenge), confirming (the
//! challenge is up), sending (Cancel works until the first request leaves, never after), and
//! done. The merge ([`super::merge`]), the auto-merge change ([`super::auto_merge`]) and the
//! re-run of failed jobs ([`super::rerun`]) are each one kind of it ([`TabAction`]); a tab runs
//! one at a time ([`TabFlow`]).
//!
//! Every step's answer is taken only under the generation it was asked for, and a flow is tied
//! to the stamp it started from: a read of the tab that shows another head, base or state while
//! the flow plans or confirms ends it before anything is sent. Nothing here sends anything
//! twice: a confirm hands out the one send of its flow, and only while the challenge is up.

use std::fmt::{Debug, Display};

use super::link::Linked;
use super::{ActionRefusal, Cancel, Decision, MergeRefusal, PullRequestService, PullRequestStamp};

/// Why a flow sent nothing, in the terms the tab shows; `R` is the guard's refusal of its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop<R> {
    /// The user cancelled before the request left.
    Cancelled,
    /// The guard refused before sending: changed since the stamp, blocked, not confirmed, a
    /// failed read, the policy.
    Refused(R),
    /// The account has no usable token.
    SignedOut {
        /// The account (`login@host`), or the host that refused the token.
        account: String,
    },
    /// Anything else before the request: the token could not be read or renewed.
    Failed(String),
}

/// What a flow needs of the guard's refusal of its kind.
pub trait Refusal: Clone + PartialEq + Eq + Debug + Display + Send + 'static {
    /// Whether it says the pull request changed since the stamp, so the tab should read it
    /// again.
    fn is_changed(&self) -> bool;
}

impl Refusal for MergeRefusal {
    fn is_changed(&self) -> bool {
        matches!(self, Self::Changed { .. })
    }
}

impl Refusal for ActionRefusal {
    fn is_changed(&self) -> bool {
        matches!(self, Self::Changed { .. })
    }
}

impl<R: Refusal> Stop<R> {
    /// Whether the refusal says the pull request changed since the stamp, so the tab should
    /// read it again.
    #[must_use]
    pub fn is_changed(&self) -> bool {
        match self {
            Self::Refused(refusal) => refusal.is_changed(),
            Self::Cancelled | Self::SignedOut { .. } | Self::Failed(_) => false,
        }
    }
}

/// Why an auto-merge change or a re-run sent nothing.
pub type ActionStop = Stop<ActionRefusal>;

/// What the service is asked to send for an auto-merge change or a re-run, with the user's
/// answer.
#[derive(Clone, Copy)]
pub struct ActionAsk<'a> {
    /// What the user saw.
    pub stamp: &'a PullRequestStamp,
    /// The decision the challenge showed; `None` when the user did not confirm.
    pub decision: Option<&'a Decision>,
    /// Called once, just before the first request leaves.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl Debug for ActionAsk<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionAsk")
            .field("stamp", self.stamp)
            .field("decision", &self.decision)
            .finish_non_exhaustive()
    }
}

/// How a flow that sends something ended, for the kinds whose outcome type says the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended<O, R> {
    /// Nothing was sent.
    NotSent(Stop<R>),
    /// The request left; what came of it.
    Sent(O),
}

/// One kind of guarded GitHub action a tab runs: what the user asked for (`Self`), the plan the
/// guard hands out, the outcome of the send, how the flow ends, and how it reaches the service.
pub trait TabAction: Clone + PartialEq + Eq + Debug + Send + Sync + 'static {
    /// What the guard's plan holds: the decision the challenge shows and the read it came from.
    type Plan: Clone + PartialEq + Eq + Debug + Send + 'static;
    /// What a send that may have left answers.
    type Outcome: Send + 'static;
    /// The guard's refusal of this kind.
    type Refusal: Refusal;
    /// How a flow of this kind ends.
    type End: Clone + PartialEq + Eq + Debug;

    /// The decision `plan`'s challenge shows.
    fn decision(plan: &Self::Plan) -> &Decision;

    /// How `answer` ends the flow.
    fn end(answer: Result<Self::Outcome, Stop<Self::Refusal>>) -> Self::End;

    /// Whether the request of a flow that ended as `end` may have reached GitHub.
    fn was_sent(end: &Self::End) -> bool;

    /// Whether `end` is a refusal saying the pull request changed since the stamp.
    fn ended_changed(end: &Self::End) -> bool;

    /// The refusal of a flow of pull request `number` whose tab read no longer matches its stamp.
    fn changed(&self, number: u64, what: &str) -> Self::Refusal;

    /// Asks the guard for the plan of `stamp` through `service`.
    ///
    /// # Errors
    /// The [`Stop`] the service answers.
    fn plan(
        &self,
        service: &dyn PullRequestService,
        linked: &Linked,
        stamp: &PullRequestStamp,
        cancel: &Cancel,
    ) -> Result<Self::Plan, Stop<Self::Refusal>>;

    /// Sends what `load` holds through `service`, once.
    ///
    /// # Errors
    /// The [`Stop`] the service answers whenever nothing was sent.
    fn send(
        service: &dyn PullRequestService,
        linked: &Linked,
        load: &SendLoad<Self>,
        on_send: &(dyn Fn() + Sync),
        cancel: &Cancel,
    ) -> Result<Self::Outcome, Stop<Self::Refusal>>;

    /// The app log's line for `flow`, about to send.
    fn attempt_log(flow: &Flow<Self>) -> String;

    /// The app log's line for how `flow` ended as `end`.
    fn outcome_log(flow: &Flow<Self>, end: &Self::End) -> String;

    /// `flow` as a tab's flow.
    fn wrap(flow: Flow<Self>) -> TabFlow;

    /// The flow of this kind `tab` holds.
    fn of(tab: &TabFlow) -> Option<&Flow<Self>>;

    /// The flow of this kind `tab` holds, to change.
    fn of_mut(tab: &mut TabFlow) -> Option<&mut Flow<Self>>;
}

/// Where one flow stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase<A: TabAction> {
    /// The guard reads the pull request again and the policy words the challenge.
    Planning,
    /// The challenge is up ([`Flow::plan`]).
    Confirming,
    /// The send runs. Until `sent`, Cancel stops it; after, nothing can.
    Sending {
        /// Whether a request left.
        sent: bool,
        /// Whether a cancel was asked for and the answer is awaited.
        cancelling: bool,
    },
    /// Finished.
    Done(A::End),
}

/// The plan read to run for a flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLoad<A> {
    /// Answer [`Flow::planned`] with it.
    pub generation: u64,
    /// What the user saw.
    pub stamp: PullRequestStamp,
    /// What the user asked for.
    pub ask: A,
}

/// The send to run for a flow, with the user's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendLoad<A> {
    /// Answer [`Flow::sent`] and [`Flow::finished`] with it.
    pub generation: u64,
    /// What the user saw.
    pub stamp: PullRequestStamp,
    /// What the user asked for.
    pub ask: A,
    /// The decision the user answered.
    pub decision: Decision,
    /// What the user typed, untouched.
    pub typed: String,
}

/// What a cancel did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelAnswer {
    /// The flow ended before anything was sent: drop it.
    Ended,
    /// The send runs and no request has left: raise its cancel flag.
    Signal,
    /// A request left: nothing can stop it.
    TooLate,
    /// The flow is done: nothing to cancel.
    Nothing,
}

/// One guarded action of one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow<A: TabAction> {
    repository: String,
    number: u64,
    generation: u64,
    stamp: PullRequestStamp,
    ask: A,
    /// The plan that landed, kept after the confirm for the log.
    plan: Option<Box<A::Plan>>,
    phase: Phase<A>,
}

impl<A: TabAction> Flow<A> {
    /// `ask` of the pull request `stamp` names in `repository` (`owner/name@host`, as the log
    /// and the notes name it for as long as the flow lives, whatever the window links to
    /// later), under `generation` (unique among the window's flows), and the plan read to run.
    #[must_use]
    pub fn begin(
        generation: u64,
        repository: String,
        stamp: PullRequestStamp,
        ask: A,
    ) -> (Self, PlanLoad<A>) {
        let load = PlanLoad {
            generation,
            stamp: stamp.clone(),
            ask: ask.clone(),
        };
        let flow = Self {
            repository,
            number: stamp.number,
            generation,
            stamp,
            ask,
            plan: None,
            phase: Phase::Planning,
        };
        (flow, load)
    }

    /// The repository it acts in, `owner/name@host`, as it was when the flow started.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    /// The generation every answer must carry.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The stamp it started from.
    #[must_use]
    pub fn stamp(&self) -> &PullRequestStamp {
        &self.stamp
    }

    /// What the user asked for.
    #[must_use]
    pub fn ask(&self) -> &A {
        &self.ask
    }

    /// Where it stands.
    #[must_use]
    pub fn phase(&self) -> &Phase<A> {
        &self.phase
    }

    /// The challenge's plan, while it is up.
    #[must_use]
    pub fn plan(&self) -> Option<&A::Plan> {
        match self.phase {
            Phase::Confirming => self.plan.as_deref(),
            Phase::Planning | Phase::Sending { .. } | Phase::Done(_) => None,
        }
    }

    /// The plan that landed, whatever the phase now; `None` before it did.
    #[must_use]
    pub fn planned_as(&self) -> Option<&A::Plan> {
        self.plan.as_deref()
    }

    /// Whether it has not finished.
    #[must_use]
    pub fn is_running(&self) -> bool {
        !matches!(self.phase, Phase::Done(_))
    }

    /// Takes the plan read of `generation`; any other is dropped. Answers whether it landed.
    pub fn planned(&mut self, generation: u64, answer: Result<A::Plan, Stop<A::Refusal>>) -> bool {
        if generation != self.generation || self.phase != Phase::Planning {
            return false;
        }
        self.phase = match answer {
            Ok(plan) => {
                self.plan = Some(Box::new(plan));
                Phase::Confirming
            }
            Err(stop) => Phase::Done(A::end(Err(stop))),
        };
        true
    }

    /// The user confirmed with `typed`: the send to run, only while the challenge is up. The
    /// text goes to the guard as typed; the guard alone verifies it.
    pub fn confirm(&mut self, typed: String) -> Option<SendLoad<A>> {
        if self.phase != Phase::Confirming {
            return None;
        }
        let plan = self.plan.as_deref()?;
        let load = SendLoad {
            generation: self.generation,
            stamp: self.stamp.clone(),
            ask: self.ask.clone(),
            decision: A::decision(plan).clone(),
            typed,
        };
        self.phase = Phase::Sending {
            sent: false,
            cancelling: false,
        };
        Some(load)
    }

    /// The request of `generation` left.
    pub fn sent(&mut self, generation: u64) {
        if generation != self.generation {
            return;
        }
        if let Phase::Sending { sent, .. } = &mut self.phase {
            *sent = true;
        }
    }

    /// Takes the send's answer of `generation`; any other is dropped. Answers whether it
    /// landed.
    pub fn finished(
        &mut self,
        generation: u64,
        answer: Result<A::Outcome, Stop<A::Refusal>>,
    ) -> bool {
        if generation != self.generation || !matches!(self.phase, Phase::Sending { .. }) {
            return false;
        }
        self.phase = Phase::Done(A::end(answer));
        true
    }

    /// The user asked to cancel.
    pub fn cancel(&mut self) -> CancelAnswer {
        match &mut self.phase {
            Phase::Planning | Phase::Confirming => {
                self.phase = Phase::Done(A::end(Err(Stop::Cancelled)));
                CancelAnswer::Ended
            }
            Phase::Sending { sent: true, .. } => CancelAnswer::TooLate,
            Phase::Sending { cancelling, .. } => {
                *cancelling = true;
                CancelAnswer::Signal
            }
            Phase::Done(_) => CancelAnswer::Nothing,
        }
    }

    /// The tab read `now` (its stamp, or `None` when the read failed): a flow that plans or
    /// confirms ends when it no longer matches, since the challenge would stand for something
    /// the user did not see. A running send is the guard's to judge. Answers whether it ended.
    pub fn follow(&mut self, now: Option<&PullRequestStamp>) -> bool {
        if !matches!(self.phase, Phase::Planning | Phase::Confirming) {
            return false;
        }
        if now == Some(&self.stamp) {
            return false;
        }
        let what = match now {
            Some(_) => "the tab read it again and it is not what the challenge was for",
            None => "the tab's read of it failed, so the screen no longer shows it as it is",
        };
        let refusal = self.ask.changed(self.number, what);
        self.phase = Phase::Done(A::end(Err(Stop::Refused(refusal))));
        true
    }
}

/// The one guarded action a tab runs, of whichever kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabFlow {
    /// A merge, plain or admin.
    Merge(super::merge::MergeFlow),
    /// Turning auto-merge on or off.
    AutoMerge(super::auto_merge::AutoMergeFlow),
    /// Re-running failed jobs.
    Rerun(super::rerun::RerunFlow),
}

impl TabFlow {
    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        match self {
            Self::Merge(flow) => flow.number(),
            Self::AutoMerge(flow) => flow.number(),
            Self::Rerun(flow) => flow.number(),
        }
    }

    /// The generation every answer must carry.
    #[must_use]
    pub fn generation(&self) -> u64 {
        match self {
            Self::Merge(flow) => flow.generation(),
            Self::AutoMerge(flow) => flow.generation(),
            Self::Rerun(flow) => flow.generation(),
        }
    }

    /// Whether it has not finished.
    #[must_use]
    pub fn is_running(&self) -> bool {
        match self {
            Self::Merge(flow) => flow.is_running(),
            Self::AutoMerge(flow) => flow.is_running(),
            Self::Rerun(flow) => flow.is_running(),
        }
    }

    /// Whether Cancel stops it now: it plans, or sends and no request has left.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        fn of<A: TabAction>(phase: &Phase<A>) -> bool {
            matches!(
                phase,
                Phase::Planning
                    | Phase::Sending {
                        sent: false,
                        cancelling: false
                    }
            )
        }
        match self {
            Self::Merge(flow) => of(flow.phase()),
            Self::AutoMerge(flow) => of(flow.phase()),
            Self::Rerun(flow) => of(flow.phase()),
        }
    }

    /// The decision the challenge shows, while it is up.
    #[must_use]
    pub fn decision(&self) -> Option<&Decision> {
        match self {
            Self::Merge(flow) => flow.plan().map(|plan| &plan.decision),
            Self::AutoMerge(flow) => flow.plan().map(|plan| &plan.decision),
            Self::Rerun(flow) => flow.plan().map(|plan| &plan.decision),
        }
    }

    /// The user asked to cancel.
    pub fn cancel(&mut self) -> CancelAnswer {
        match self {
            Self::Merge(flow) => flow.cancel(),
            Self::AutoMerge(flow) => flow.cancel(),
            Self::Rerun(flow) => flow.cancel(),
        }
    }

    /// The tab read `now`; see [`Flow::follow`].
    pub fn follow(&mut self, now: Option<&PullRequestStamp>) -> bool {
        match self {
            Self::Merge(flow) => flow.follow(now),
            Self::AutoMerge(flow) => flow.follow(now),
            Self::Rerun(flow) => flow.follow(now),
        }
    }

    /// The request of `generation` left.
    pub fn sent(&mut self, generation: u64) {
        match self {
            Self::Merge(flow) => flow.sent(generation),
            Self::AutoMerge(flow) => flow.sent(generation),
            Self::Rerun(flow) => flow.sent(generation),
        }
    }
}
