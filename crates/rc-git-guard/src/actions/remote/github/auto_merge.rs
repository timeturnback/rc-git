//! Turning GitHub's auto-merge on and off for a pull request. Once on, GitHub merges the pull
//! request by itself when its requirements are met, with whatever was pushed to its head branch
//! by then, and nobody is asked again: so turning it on is confirmed like a merge, and the
//! challenge says so on a line of its own.
//!
//! The sequence is the merge's ([`super::merge`]), inside [`set_auto_merge`]:
//!
//! 1. Read the pull request again and compare it with the stamp of what the user saw.
//! 2. Refuse what the change cannot do ([`enable_auto_merge_blocker`],
//!    [`disable_auto_merge_blocker`]). Turning it on needs an open pull request that branch
//!    protection blocks (a blocked or behind merge state, as the plain merge reads it: one that
//!    can merge now is told to use Merge), a method the repository allows, a repository that
//!    allows auto-merge, an account GitHub says may turn it on, and auto-merge off. Turning it
//!    off needs it on and an account GitHub says may turn it off.
//! 3. Build the action from that read and authorize it: the decision the user answered must be
//!    the policy's own now.
//! 4. Send the one mutation, built from the proof alone: `enablePullRequestAutoMerge` with the
//!    expected head and the method, or `disablePullRequestAutoMerge`. Never sent again.
//! 5. Read the outcome as done, refused, or unknown; only a lost answer reads the pull request
//!    once more, to settle what it can: on means an auto-merge request with the confirmed
//!    method at the confirmed head, off means none, and anything else stays unknown.

use rc_git_core::Oid;
use serde_json::Value;

use super::api::{Mutated, Session};
use super::guarded::{
    ActionBlocker, ActionPlan, ActionRefusal, ActionTarget, PullRequestAction, PullRequestFacts,
    plan_action, proof_for, settling,
};
use super::merge::{
    FirstLook, LOGIN_CHARS, MergeBlocker, Settled, UNDO, checks_phrase, errors_verdict, fact,
    fact_line, first_look, merge_blocker, method_phrase, oid_at, quoted,
};
use super::mutation::method_word;
use super::pull_types::{
    CheckTally, MergeMethod, PullRequestDetail, PullRequestId, PullRequestStamp, PullRequestState,
};
use super::pulls::{MAX_LINE_CHARS, read_pull_request};
use super::text::single_line;
use super::{GitHubError, RepositoryTarget};
use crate::actions::remote::Cancel;
use crate::{Action, Decision, Policy};

/// What a challenge to turn auto-merge on says GitHub will do once it is on.
const MERGES_BY_ITSELF: &str = "GitHub will merge this pull request by itself when its \
                                requirements are met, including commits pushed to its head \
                                branch after this confirmation; rc-git will not ask again";

/// Turning auto-merge on as the user confirms it: every fact its challenge names, read from the
/// host just before, and the node id the mutation names it by. Built only by
/// [`AutoMergeEnable::new`], from a read that nothing in [`enable_auto_merge_blocker`] stands in
/// the way of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMergeEnable {
    facts: PullRequestFacts,
    id: PullRequestId,
    method: MergeMethod,
    checks: CheckTally,
}

impl AutoMergeEnable {
    /// Turning on auto-merge for `detail`, as read from `target` with the token of account
    /// `login`, to merge with `method`.
    ///
    /// # Errors
    /// The [`ActionBlocker`] [`enable_auto_merge_blocker`] finds.
    pub fn new(
        target: &RepositoryTarget,
        login: &str,
        detail: &PullRequestDetail,
        method: MergeMethod,
    ) -> Result<Self, ActionBlocker> {
        if let Some(blocker) = enable_auto_merge_blocker(detail, method) {
            return Err(blocker);
        }
        let id = detail
            .id
            .clone()
            .ok_or(ActionBlocker::Merge(MergeBlocker::NoId))?;
        Ok(Self {
            facts: PullRequestFacts::read(target, login, detail),
            id,
            method,
            checks: detail.checks.tally(),
        })
    }

    /// The repository.
    #[must_use]
    pub fn target(&self) -> &RepositoryTarget {
        self.facts.target()
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.facts.number()
    }

    pub(super) fn id(&self) -> &PullRequestId {
        &self.id
    }

    /// The head commit the change expects.
    #[must_use]
    pub fn head_oid(&self) -> Oid {
        self.facts.head_oid()
    }

    /// How GitHub will merge it.
    #[must_use]
    pub fn method(&self) -> MergeMethod {
        self.method
    }

    /// `owner/name#number`.
    #[must_use]
    pub fn label(&self) -> String {
        self.facts.label()
    }

    /// The challenge's text: rc-git's own first line, the repository to the head, the method
    /// and the checks, the line that says GitHub merges it by itself with later commits too, the
    /// owner of a head branch in another owner's repository, and the title last.
    pub(crate) fn summary(&self) -> String {
        let mut lines = vec![format!(
            "Turn on auto-merge for this pull request on GitHub. {UNDO}"
        )];
        lines.extend(self.facts.lines());
        lines.push(fact_line("Method", method_phrase(self.method)));
        lines.push(fact_line("Checks", &checks_phrase(&self.checks)));
        lines.push(fact_line("Merges by itself", MERGES_BY_ITSELF));
        if let Some(fork) = self.facts.fork() {
            lines.push(fact_line(
                "Fork",
                &format!(
                    "its head branch is in {}, who can push more commits to it",
                    fork.owner_phrase()
                ),
            ));
        }
        lines.push(self.facts.title_line());
        lines.join("\n")
    }
}

/// Turning auto-merge off as the user confirms it: the facts its challenge names, with the
/// auto-merge request the read found. Built only by [`AutoMergeDisable::new`], from a read that
/// nothing in [`disable_auto_merge_blocker`] stands in the way of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMergeDisable {
    facts: PullRequestFacts,
    id: PullRequestId,
    /// The method the auto-merge request names, when the host named one this build knows.
    method: Option<MergeMethod>,
    /// Who turned it on, neutralized.
    enabled_by: Option<String>,
}

impl AutoMergeDisable {
    /// Turning off auto-merge for `detail`, as read from `target` with the token of account
    /// `login`.
    ///
    /// # Errors
    /// The [`ActionBlocker`] [`disable_auto_merge_blocker`] finds.
    pub fn new(
        target: &RepositoryTarget,
        login: &str,
        detail: &PullRequestDetail,
    ) -> Result<Self, ActionBlocker> {
        if let Some(blocker) = disable_auto_merge_blocker(detail) {
            return Err(blocker);
        }
        let (Some(id), Some(auto)) = (detail.id.clone(), detail.auto_merge.as_ref()) else {
            return Err(ActionBlocker::Merge(MergeBlocker::NoId));
        };
        Ok(Self {
            facts: PullRequestFacts::read(target, login, detail),
            id,
            method: auto.method,
            enabled_by: auto
                .enabled_by
                .as_deref()
                .map(|login| fact(login, LOGIN_CHARS)),
        })
    }

    /// The repository.
    #[must_use]
    pub fn target(&self) -> &RepositoryTarget {
        self.facts.target()
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.facts.number()
    }

    pub(super) fn id(&self) -> &PullRequestId {
        &self.id
    }

    /// The head commit the read showed.
    #[must_use]
    pub fn head_oid(&self) -> Oid {
        self.facts.head_oid()
    }

    /// `owner/name#number`.
    #[must_use]
    pub fn label(&self) -> String {
        self.facts.label()
    }

    /// The challenge's text: rc-git's own first line, the repository to the head, the
    /// auto-merge request as read, and the title last.
    pub(crate) fn summary(&self) -> String {
        let method = self
            .method
            .map_or("a method rc-git does not know", method_phrase);
        let by = self
            .enabled_by
            .as_deref()
            .map_or_else(|| "someone GitHub did not name".to_owned(), quoted);
        let mut lines = vec![
            "Turn off auto-merge for this pull request on GitHub. GitHub then no longer merges it \
             by itself; nothing else changes."
                .to_owned(),
        ];
        lines.extend(self.facts.lines());
        lines.push(fact_line(
            "Auto-merge",
            &format!("on, as {method}, turned on by {by}"),
        ));
        lines.push(self.facts.title_line());
        lines.join("\n")
    }
}

/// What stands in the way of turning on auto-merge for `detail` with `method`; `None` when
/// nothing does. The one rule the control and the change itself both follow. A pull request
/// the plain merge would refuse for another reason than branch protection is refused for that
/// reason; one it can take now is [`ActionBlocker::CanMergeNow`], since GitHub shows auto-merge
/// only for pull requests that cannot merge yet.
#[must_use]
pub fn enable_auto_merge_blocker(
    detail: &PullRequestDetail,
    method: MergeMethod,
) -> Option<ActionBlocker> {
    match merge_blocker(detail, method) {
        None => return Some(ActionBlocker::CanMergeNow),
        Some(MergeBlocker::Blocked | MergeBlocker::Behind) => {}
        Some(
            blocker @ (MergeBlocker::NotOpen(_)
            | MergeBlocker::Draft
            | MergeBlocker::MethodNotAllowed(_)
            | MergeBlocker::Conflicting
            | MergeBlocker::MergeabilityUnknown
            | MergeBlocker::MergeStateUnknown
            | MergeBlocker::MergeStateUnreported
            | MergeBlocker::NoId
            | MergeBlocker::NothingToBypass
            | MergeBlocker::NoAdminRight),
        ) => return Some(ActionBlocker::Merge(blocker)),
    }
    if detail.auto_merge.is_some() {
        return Some(ActionBlocker::AutoMergeOn);
    }
    match detail.auto_merge_allowed {
        Some(true) => {}
        Some(false) => return Some(ActionBlocker::AutoMergeNotAllowed),
        None => return Some(ActionBlocker::AutoMergeUnreported),
    }
    if !detail
        .viewer
        .is_some_and(|viewer| viewer.can_enable_auto_merge)
    {
        return Some(ActionBlocker::NoEnableRight);
    }
    detail
        .id
        .is_none()
        .then_some(ActionBlocker::Merge(MergeBlocker::NoId))
}

/// What stands in the way of turning off auto-merge for `detail`; `None` when nothing does.
#[must_use]
pub fn disable_auto_merge_blocker(detail: &PullRequestDetail) -> Option<ActionBlocker> {
    let state = detail.summary.state;
    if state != PullRequestState::Open {
        return Some(ActionBlocker::Merge(MergeBlocker::NotOpen(state)));
    }
    if detail.auto_merge.is_none() {
        return Some(ActionBlocker::AutoMergeOff);
    }
    if !detail
        .viewer
        .is_some_and(|viewer| viewer.can_disable_auto_merge)
    {
        return Some(ActionBlocker::NoDisableRight);
    }
    detail
        .id
        .is_none()
        .then_some(ActionBlocker::Merge(MergeBlocker::NoId))
}

/// Which way the user asked to change auto-merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoMergeChange {
    /// Turn it on, to merge with this method.
    Enable(MergeMethod),
    /// Turn it off.
    Disable,
}

impl AutoMergeChange {
    fn action(self) -> PullRequestAction {
        match self {
            Self::Enable(_) => PullRequestAction::EnableAutoMerge,
            Self::Disable => PullRequestAction::DisableAutoMerge,
        }
    }

    /// The action this change makes of `detail`, read from `target` by `login`.
    fn build(
        self,
        target: &RepositoryTarget,
        login: &str,
        detail: &PullRequestDetail,
    ) -> Result<Action, ActionBlocker> {
        Ok(match self {
            Self::Enable(method) => Action::EnableAutoMerge(Box::new(AutoMergeEnable::new(
                target, login, detail, method,
            )?)),
            Self::Disable => {
                Action::DisableAutoMerge(Box::new(AutoMergeDisable::new(target, login, detail)?))
            }
        })
    }
}

/// An auto-merge change to run.
#[derive(Clone, Copy)]
pub struct AutoMergeRequest<'a> {
    /// Which pull request, by which account.
    pub who: ActionTarget<'a>,
    /// On or off.
    pub change: AutoMergeChange,
    /// The decision the challenge showed, as [`plan_auto_merge`] handed it out; `None` when the
    /// user did not confirm, which sends nothing.
    pub answer: Option<&'a Decision>,
    /// Called once, just before the request leaves, after the last point a cancel stops it.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for AutoMergeRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoMergeRequest")
            .field("who", &self.who)
            .field("change", &self.change)
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

/// Where an auto-merge change stands once its request was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoMergeOutcome {
    /// GitHub did as asked: auto-merge is on with the confirmed method at the confirmed head, or
    /// off.
    Done {
        /// How rc-git knows.
        settled: Settled,
    },
    /// GitHub turned it on, at the confirmed head with the confirmed method, for a pull request
    /// whose base is no longer the one confirmed: it was retargeted after rc-git's last read, so
    /// it will merge into that base. Never reported as plain success.
    OnOtherBase {
        /// The base the user confirmed.
        confirmed: String,
        /// The base GitHub names now, single-line.
        actual: String,
        /// How rc-git knows.
        settled: Settled,
    },
    /// GitHub answered and refused; nothing changed.
    Refused {
        /// GitHub's reason, single-line and scrubbed.
        reason: String,
    },
    /// The request left and no usable answer came back; the read after it says what it can.
    Unknown {
        /// What went wrong with the answer.
        problem: String,
        /// What reading the pull request again found.
        after: AutoMergeAfter,
    },
}

/// What the read after a lost answer found, when it settles nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoMergeAfter {
    /// Open at the confirmed head with auto-merge on, with this method (`None`: one this build
    /// does not know).
    On {
        /// The method the auto-merge request names.
        method: Option<MergeMethod>,
    },
    /// Open with auto-merge off.
    Off,
    /// Open, at another head than the confirmed one.
    OtherHead {
        /// The head it is open at.
        head: Oid,
    },
    /// Merged: rc-git cannot tell by what.
    Merged,
    /// Closed without merging.
    Closed,
    /// The read failed too, or answered a state this build does not know.
    Unread {
        /// Why.
        reason: String,
    },
}

/// What the challenge must show for changing auto-merge as `change` says on the pull request
/// `who` names: reads it now, refuses when it changed since the stamp or the change cannot be
/// made, and answers the policy's decision for that read. Sends nothing but the read.
///
/// # Errors
/// [`ActionRefusal::Read`], [`ActionRefusal::Cancelled`], [`ActionRefusal::Changed`],
/// [`ActionRefusal::Blocked`], and [`ActionRefusal::Denied`] when the policy refuses it.
pub fn plan_auto_merge(
    policy: &Policy,
    who: &ActionTarget<'_>,
    change: AutoMergeChange,
    cancel: &Cancel,
) -> Result<ActionPlan, ActionRefusal> {
    plan_action(
        policy,
        who,
        change.action(),
        |detail| change.build(who.target, who.login, detail),
        cancel,
    )
}

/// Changes auto-merge as `request` says, in the module's sequence: a fresh read compared with
/// the stamp, the blockers, the policy's own decision checked against the answered one, then one
/// mutation. A cancel before the request leaves sends nothing; after, it cannot stop it.
///
/// # Errors
/// An [`ActionRefusal`] whenever nothing was sent; once the request may have left, the answer
/// is an [`AutoMergeOutcome`], never an error.
pub fn set_auto_merge(
    policy: &Policy,
    request: AutoMergeRequest<'_>,
    cancel: &Cancel,
) -> Result<AutoMergeOutcome, ActionRefusal> {
    let who = &request.who;
    let change = request.change;
    let action = change.action();
    let pull_request = who.pull_request();
    let proof = proof_for(
        policy,
        who,
        (action, request.answer),
        |detail| change.build(who.target, who.login, detail),
        cancel,
    )?;
    (request.on_send)();
    let not_sent = |source| match source {
        GitHubError::Cancelled { .. } => ActionRefusal::Cancelled {
            action,
            pull_request: pull_request.clone(),
        },
        source => ActionRefusal::NotSent {
            action,
            pull_request: pull_request.clone(),
            source,
        },
    };
    let mut outbox = Session::new(who.host, who.token)
        .mutate(proof)
        .map_err(not_sent)?;
    let problem = match outbox.send_next(cancel) {
        None => {
            return Err(not_sent(GitHubError::InvalidValue {
                field: "GitHub mutation",
                value: "the confirmed auto-merge change".to_owned(),
                reason: "it names no request to send",
            }));
        }
        Some(Mutated::NotSent(source)) => return Err(not_sent(source)),
        Some(Mutated::Answered(answer)) => match answered(first_look(answer), change, who.stamp) {
            Ok(outcome) => return Ok(outcome),
            Err(problem) => problem,
        },
        Some(Mutated::Lost(error)) => error.to_string(),
    };
    Ok(settle(who, change, problem, cancel))
}

/// The payload key of each change's answer.
fn payload_key(change: AutoMergeChange) -> &'static str {
    match change {
        AutoMergeChange::Enable(_) => "enablePullRequestAutoMerge",
        AutoMergeChange::Disable => "disablePullRequestAutoMerge",
    }
}

/// What the host's answer says: a final outcome, or the problem to report beside the read that
/// follows. The answer's `data` is read first; only an empty payload with errors that each show
/// the mutation did not run or was refused is a refusal.
fn answered(
    look: FirstLook,
    change: AutoMergeChange,
    stamp: &PullRequestStamp,
) -> Result<AutoMergeOutcome, String> {
    let reply = match look {
        FirstLook::Reply(reply) => reply,
        FirstLook::Refused(reason) => return Ok(AutoMergeOutcome::Refused { reason }),
        FirstLook::Unknown(problem) => return Err(problem),
    };
    let payload = reply
        .data
        .as_ref()
        .and_then(|data| data.get(payload_key(change)))
        .filter(|payload| !payload.is_null());
    let pull_request = payload.and_then(|payload| payload.get("pullRequest"));
    if let Some(outcome) = pull_request.and_then(|node| done(node, change, stamp)) {
        return Ok(outcome);
    }
    let words = match change {
        AutoMergeChange::Enable(_) => ("auto-merge is on", "that auto-merge is on"),
        AutoMergeChange::Disable => ("auto-merge is off", "that auto-merge is off"),
    };
    errors_verdict(&reply, payload.is_some(), words)
        .map(|reason| AutoMergeOutcome::Refused { reason })
}

/// The outcome the answer's `pullRequest` node shows, when it shows the change made: open at the
/// confirmed head with an auto-merge request of the confirmed method (on), or open with an
/// auto-merge request that is present and null (off).
fn done(
    node: &Value,
    change: AutoMergeChange,
    stamp: &PullRequestStamp,
) -> Option<AutoMergeOutcome> {
    let open = node.get("state").and_then(Value::as_str) == Some("OPEN");
    let auto = node.get("autoMergeRequest")?;
    match change {
        AutoMergeChange::Enable(method) => {
            let at_head = oid_at(node, &["headRefOid"]) == Some(stamp.head_oid);
            let named = auto.get("mergeMethod").and_then(Value::as_str);
            if !open || !at_head || named != Some(method_word(method)) {
                return None;
            }
            let base = node
                .get("baseRefName")
                .and_then(Value::as_str)
                .map(|base| single_line(base, MAX_LINE_CHARS));
            Some(on_base(stamp, base, Settled::Answer))
        }
        AutoMergeChange::Disable => (open && auto.is_null()).then_some(AutoMergeOutcome::Done {
            settled: Settled::Answer,
        }),
    }
}

/// Auto-merge on, at the confirmed head, for a pull request GitHub names on `base` (`None`: it
/// did not say).
fn on_base(stamp: &PullRequestStamp, base: Option<String>, settled: Settled) -> AutoMergeOutcome {
    match base {
        Some(actual) if actual != stamp.base_ref => AutoMergeOutcome::OnOtherBase {
            confirmed: stamp.base_ref.clone(),
            actual,
            settled,
        },
        Some(_) | None => AutoMergeOutcome::Done { settled },
    }
}

/// The outcome of a request whose answer was lost: one read of the pull request, never a second
/// mutation.
fn settle(
    who: &ActionTarget<'_>,
    change: AutoMergeChange,
    problem: String,
    cancel: &Cancel,
) -> AutoMergeOutcome {
    let read = read_pull_request(
        who.host,
        who.token,
        who.target,
        who.stamp.number,
        &settling(cancel),
    );
    let detail = match read {
        Ok(detail) => detail,
        Err(error) => {
            return AutoMergeOutcome::Unknown {
                problem,
                after: AutoMergeAfter::Unread {
                    reason: error.to_string(),
                },
            };
        }
    };
    let after = after_loss(&detail, who.stamp, change);
    let settled = match change {
        AutoMergeChange::Enable(method) => {
            after
                == AutoMergeAfter::On {
                    method: Some(method),
                }
        }
        AutoMergeChange::Disable => after == AutoMergeAfter::Off,
    };
    if !settled {
        return AutoMergeOutcome::Unknown { problem, after };
    }
    match change {
        AutoMergeChange::Enable(_) => on_base(
            who.stamp,
            Some(detail.summary.base_ref.clone()),
            Settled::Reread,
        ),
        AutoMergeChange::Disable => AutoMergeOutcome::Done {
            settled: Settled::Reread,
        },
    }
}

/// Where `detail` stands after a lost answer of `change`, against the confirmed `stamp`. Turning
/// it on is about the confirmed head, so another head is its own answer; turning it off is not,
/// and an open pull request with auto-merge off is off whatever its head.
fn after_loss(
    detail: &PullRequestDetail,
    stamp: &PullRequestStamp,
    change: AutoMergeChange,
) -> AutoMergeAfter {
    let head = detail.summary.head_oid;
    let moved = head != stamp.head_oid && matches!(change, AutoMergeChange::Enable(_));
    match detail.summary.state {
        PullRequestState::Merged => AutoMergeAfter::Merged,
        PullRequestState::Closed => AutoMergeAfter::Closed,
        PullRequestState::Unknown => AutoMergeAfter::Unread {
            reason: "the host reported a state rc-git does not know".to_owned(),
        },
        PullRequestState::Open if moved => AutoMergeAfter::OtherHead { head },
        PullRequestState::Open => match &detail.auto_merge {
            Some(auto) => AutoMergeAfter::On {
                method: auto.method,
            },
            None => AutoMergeAfter::Off,
        },
    }
}
