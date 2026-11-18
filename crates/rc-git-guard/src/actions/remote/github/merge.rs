//! Merging a pull request on GitHub: the first rc-git action that changes something outside the
//! machine, and one rc-git cannot take back. A merge lands on a branch everyone shares, so this
//! module refuses whenever it cannot be sure, and sends one request at most.
//!
//! The sequence, inside [`merge_pull_request`] (the app and the CLI both call it):
//!
//! 1. Read the pull request again now and compare it with the [`PullRequestStamp`] of what the
//!    user saw: a head, base or state that differs refuses with "changed since you looked".
//! 2. Refuse what cannot merge as asked. The plain merge ([`MergeKind::Plain`],
//!    [`merge_blocker`]) refuses a pull request that is not open, a draft, a method the
//!    repository does not allow, conflicts, and any merge state but clean, unstable or
//!    has-hooks. The admin merge ([`MergeKind::Admin`], [`admin_merge_blocker`]) takes only a
//!    blocked or behind state the account may merge past (`viewerCanMergeAsAdmin`), and
//!    refuses everything else, a pull request the plain merge can take included.
//! 3. Build the [`Action`] from that read and authorize it: the decision the user answered must
//!    be the one the policy gives now (a challenge for anything else, a pull request whose
//!    checks or title moved since the challenge was shown, refuses), and no answer at all
//!    refuses. The admin merge's decision also holds what it skips, and its challenge needs the
//!    pull request's number typed.
//! 4. Send exactly one mutation, built from the proof alone ([`super::mutation`]), carrying the
//!    expected head and the method. It is never sent again, at any layer.
//! 5. Read the outcome as one of three ([`MergeOutcome`]): merged (GitHub said so), refused
//!    (GitHub answered no), or unknown (the request left and no usable answer came back). Only
//!    the unknown case reads the pull request once more, to settle what it can.
//!
//! Every refusal before step 4 is a [`MergeRefusal`] and means nothing was sent.

mod admin;

use rc_git_core::Oid;
use serde_json::Value;

use super::api::{GraphqlError, GraphqlReply, Mutated, Session};
use super::pull_types::{
    CheckTally, MergeMethod, MergeStateStatus, Mergeable, PullRequestDetail, PullRequestId,
    PullRequestStamp, PullRequestState,
};
use super::pulls::read_pull_request;
use super::secret::ApiToken;
use super::text::single_line;
use super::{GitHubError, GitHubHost, RepositoryTarget};
use crate::actions::remote::Cancel;
use crate::{Action, Confirmation, Decision, Policy, Refusal};

pub use admin::{Bypassed, PullRequestAdminMerge, admin_merge_blocker};

/// The longest title a challenge quotes, in characters.
const TITLE_CHARS: usize = 120;

/// The longest branch name a challenge quotes, in characters.
const BRANCH_CHARS: usize = 255;

/// A pull request merge as the user confirms it: every fact its challenge names, read from the
/// host just before, and the node id the mutation names it by. Built only by
/// [`PullRequestMerge::new`], from a read that nothing stands in the way of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestMerge {
    target: RepositoryTarget,
    number: u64,
    id: PullRequestId,
    title: String,
    base: String,
    head: String,
    head_oid: Oid,
    method: MergeMethod,
    checks: CheckTally,
}

impl PullRequestMerge {
    /// The merge of `detail`, as read from `target`, with `method`.
    ///
    /// # Errors
    /// The [`MergeBlocker`] [`merge_blocker`] finds.
    pub fn new(
        target: &RepositoryTarget,
        detail: &PullRequestDetail,
        method: MergeMethod,
    ) -> Result<Self, MergeBlocker> {
        if let Some(blocker) = merge_blocker(detail, method) {
            return Err(blocker);
        }
        Self::read(target, detail, method)
    }

    /// The facts of `detail` a merge names, whatever stands in its way: only for a constructor
    /// that has already decided the merge may be offered ([`PullRequestMerge::new`], and
    /// [`PullRequestAdminMerge::new`] for the merge it wraps). Private to this module and the
    /// admin merge's, so no other code builds a merge of a read its blockers did not pass.
    fn read(
        target: &RepositoryTarget,
        detail: &PullRequestDetail,
        method: MergeMethod,
    ) -> Result<Self, MergeBlocker> {
        let id = detail.id.clone().ok_or(MergeBlocker::NoId)?;
        let summary = &detail.summary;
        Ok(Self {
            target: target.clone(),
            number: summary.number,
            id,
            title: single_line(&summary.title, TITLE_CHARS),
            base: single_line(&summary.base_ref, BRANCH_CHARS),
            head: single_line(&summary.head_ref, BRANCH_CHARS),
            head_oid: summary.head_oid,
            method,
            checks: detail.checks.tally(),
        })
    }

    /// The repository.
    #[must_use]
    pub fn target(&self) -> &RepositoryTarget {
        &self.target
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }

    pub(super) fn id(&self) -> &PullRequestId {
        &self.id
    }

    /// The head commit the merge expects.
    #[must_use]
    pub fn head_oid(&self) -> Oid {
        self.head_oid
    }

    /// How it merges.
    #[must_use]
    pub fn method(&self) -> MergeMethod {
        self.method
    }

    /// `owner/name#number`.
    #[must_use]
    pub fn label(&self) -> String {
        label(&self.target, self.number)
    }

    /// The challenge's sentence: the pull request, its title, both branches, the head commit,
    /// the method and the checks, and that rc-git cannot undo it.
    pub(crate) fn summary(&self) -> String {
        let short: String = self.head_oid.to_hex().chars().take(7).collect();
        format!(
            "Merge {} \u{201c}{}\u{201d} into {} with {}: {} at {short} ({}). GitHub merges it \
             for everyone and rc-git cannot undo it; the local repository is unchanged until you \
             fetch.",
            self.label(),
            self.title,
            self.base,
            method_phrase(self.method),
            self.head,
            checks_phrase(&self.checks),
        )
    }
}

/// `owner/name#number`.
fn label(target: &RepositoryTarget, number: u64) -> String {
    format!("{}/{}#{number}", target.owner(), target.name())
}

fn method_phrase(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "a merge commit",
        MergeMethod::Squash => "a squash merge",
        MergeMethod::Rebase => "a rebase merge",
    }
}

/// `3 of 5 checks passed, 1 failed, 1 pending`.
fn checks_phrase(tally: &CheckTally) -> String {
    if tally.total == 0 {
        return "no checks".to_owned();
    }
    let mut phrase = format!("{} of {} checks passed", tally.passing, tally.total);
    let counted = tally.passing + tally.failing + tally.pending + tally.neutral;
    for (count, word) in [
        (tally.failing, "failed"),
        (tally.pending, "pending"),
        (tally.neutral, "neutral or skipped"),
        (tally.total.saturating_sub(counted), "not read"),
    ] {
        if count > 0 {
            phrase = format!("{phrase}, {count} {word}");
        }
    }
    phrase
}

/// What stands between a pull request and a plain merge, as rc-git reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum MergeBlocker {
    /// It is not open: closed, merged, or in a state this build does not know.
    #[error("it is {}", state_word(*.0))]
    NotOpen(PullRequestState),
    /// A draft.
    #[error("it is a draft; mark it ready for review first")]
    Draft,
    /// The repository does not allow this method.
    #[error("the repository does not allow {}", method_phrase(*.0))]
    MethodNotAllowed(MergeMethod),
    /// It conflicts with its base.
    #[error("it conflicts with its base branch")]
    Conflicting,
    /// GitHub has not finished checking it for conflicts.
    #[error("GitHub has not finished checking it for conflicts; try again in a moment")]
    MergeabilityUnknown,
    /// Branch protection blocks the merge.
    #[error("branch protection blocks the merge")]
    Blocked,
    /// Its head branch is out of date with its base.
    #[error("its head branch is out of date with its base; update it first")]
    Behind,
    /// GitHub has not worked out whether it can merge.
    #[error("GitHub has not worked out yet whether it can merge; try again in a moment")]
    MergeStateUnknown,
    /// The host does not report the merge state (an older GitHub Enterprise Server), so rc-git
    /// cannot tell whether branch protection allows the merge.
    #[error(
        "this host does not report whether branch protection allows the merge; merge it on \
         the host's own page"
    )]
    MergeStateUnreported,
    /// The host sent no node id for it, so no mutation can name it.
    #[error("the host sent no id for it, so rc-git cannot name it in a merge")]
    NoId,
    /// An admin merge of a pull request the plain merge can take: nothing blocks it, so there
    /// is no protection to merge past.
    #[error("nothing blocks it, so there is no branch protection to merge past; use Merge")]
    NothingToBypass,
    /// An admin merge GitHub does not say the account may make: `viewerCanMergeAsAdmin` is
    /// false, or the host did not say.
    #[error("GitHub does not report that your account may merge it past branch protection")]
    NoAdminRight,
}

fn state_word(state: PullRequestState) -> &'static str {
    match state {
        PullRequestState::Open => "open",
        PullRequestState::Closed => "closed",
        PullRequestState::Merged => "already merged",
        PullRequestState::Unknown => "in a state rc-git does not know",
    }
}

impl super::pull_types::MergeMethods {
    /// Whether the repository allows `method`.
    #[must_use]
    pub fn allows(&self, method: MergeMethod) -> bool {
        match method {
            MergeMethod::Merge => self.merge,
            MergeMethod::Squash => self.squash,
            MergeMethod::Rebase => self.rebase,
        }
    }
}

/// What stands in the way of merging `detail` with `method`, in the order a user fixes them;
/// `None` when nothing does. The one rule the merge button and the merge itself both follow.
#[must_use]
pub fn merge_blocker(detail: &PullRequestDetail, method: MergeMethod) -> Option<MergeBlocker> {
    let summary = &detail.summary;
    if summary.state != PullRequestState::Open {
        return Some(MergeBlocker::NotOpen(summary.state));
    }
    if summary.draft {
        return Some(MergeBlocker::Draft);
    }
    if !detail.merge_methods.allows(method) {
        return Some(MergeBlocker::MethodNotAllowed(method));
    }
    match detail.mergeable {
        Mergeable::Mergeable => {}
        Mergeable::Conflicting => return Some(MergeBlocker::Conflicting),
        Mergeable::Unknown => return Some(MergeBlocker::MergeabilityUnknown),
    }
    if let Some(blocker) = merge_state_blocker(detail.merge_state) {
        return Some(blocker);
    }
    detail.id.is_none().then_some(MergeBlocker::NoId)
}

/// What a merge state rules out. Only the states GitHub calls mergeable pass; a host that
/// reports none passes nothing.
fn merge_state_blocker(state: Option<MergeStateStatus>) -> Option<MergeBlocker> {
    let Some(state) = state else {
        return Some(MergeBlocker::MergeStateUnreported);
    };
    match state {
        MergeStateStatus::Clean | MergeStateStatus::HasHooks | MergeStateStatus::Unstable => None,
        MergeStateStatus::Blocked => Some(MergeBlocker::Blocked),
        MergeStateStatus::Behind => Some(MergeBlocker::Behind),
        MergeStateStatus::Dirty => Some(MergeBlocker::Conflicting),
        MergeStateStatus::Draft => Some(MergeBlocker::Draft),
        MergeStateStatus::Unknown => Some(MergeBlocker::MergeStateUnknown),
    }
}

/// Which merge the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeKind {
    /// The plain merge: only a pull request nothing stands in the way of ([`merge_blocker`]).
    Plain,
    /// The admin merge: a pull request branch protection blocks, past that protection, when
    /// GitHub says the account may ([`admin_merge_blocker`]). Its challenge asks for the pull
    /// request's number, typed.
    Admin,
}

/// Which pull request to merge and how, with the account that merges it.
#[derive(Debug, Clone, Copy)]
pub struct MergeTarget<'a> {
    /// The account's host: the only one the token and the mutation go to.
    pub host: &'a GitHubHost,
    /// The account's token.
    pub token: &'a ApiToken,
    /// The repository, which must be on `host`.
    pub target: &'a RepositoryTarget,
    /// What the user saw of the pull request.
    pub stamp: &'a PullRequestStamp,
    /// The method the user chose.
    pub method: MergeMethod,
    /// The plain merge or the admin merge, as the user chose.
    pub kind: MergeKind,
}

/// The user's answer to a merge's challenge.
#[derive(Debug, Clone, Copy)]
pub struct MergeAnswer<'a> {
    /// The decision the challenge showed, as [`plan_merge_pull_request`] handed it out.
    pub decision: &'a Decision,
    /// What the user typed in the challenge's field, untouched; empty for a plain confirm.
    pub typed: &'a str,
}

/// A merge to run.
#[derive(Clone, Copy)]
pub struct MergeRequest<'a> {
    /// What to merge.
    pub merge: MergeTarget<'a>,
    /// The user's answer; `None` when the user did not confirm, which sends nothing.
    pub answer: Option<MergeAnswer<'a>>,
    /// Called once, just before the request leaves, after the last point a cancel stops it:
    /// the front end stops offering Cancel then.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for MergeRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergeRequest")
            .field("merge", &self.merge)
            .field("answer", &self.answer)
            .finish_non_exhaustive()
    }
}

/// What a merge needs the user to answer, from a read made just now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePlan {
    /// The policy's decision for the merge as read: what the challenge shows.
    pub decision: Decision,
    /// The pull request as read.
    pub detail: PullRequestDetail,
    /// What an admin merge skips, as that read reports it; `None` for the plain merge.
    pub bypassed: Option<Bypassed>,
}

/// Where a merge stands once its request was sent: never two-valued, since a request whose
/// answer is lost may or may not have merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// GitHub merged it.
    Merged {
        /// The merge commit, when GitHub named one.
        merge_commit: Option<Oid>,
        /// How rc-git knows.
        settled: Settled,
    },
    /// GitHub answered and refused; nothing merged.
    Refused {
        /// GitHub's reason, single-line and scrubbed.
        reason: String,
    },
    /// The request left and no usable answer came back; the read after it says what it can.
    Unknown {
        /// What went wrong with the answer.
        problem: String,
        /// What reading the pull request again found.
        after: AfterLoss,
    },
}

/// How rc-git knows a pull request merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Settled {
    /// GitHub's answer to the merge said so.
    Answer,
    /// The answer was lost, and a read afterwards found it merged at the confirmed head.
    Reread,
}

/// What the read after a lost answer found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterLoss {
    /// Still open, at `head`. The merge may still land, or may never have arrived; the read
    /// cannot tell the two apart.
    StillOpen {
        /// The head it is open at.
        head: Oid,
    },
    /// Closed without merging: this request merged nothing.
    Closed,
    /// Merged, but at another head than the confirmed one: not by this request, which named the
    /// confirmed head.
    MergedAtOtherHead {
        /// The head it merged at.
        head: Oid,
    },
    /// The read failed too, or answered a state this build does not know.
    Unread {
        /// Why.
        reason: String,
    },
}

/// Why a merge sent nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeRefusal {
    /// The read before the merge failed.
    #[error("merge {pull_request}: nothing was sent, because reading it first failed: {source}")]
    Read {
        /// `owner/name#number`.
        pull_request: String,
        /// The read's failure.
        source: GitHubError,
    },
    /// It changed since the user looked.
    #[error("merge {pull_request}: it changed since you looked ({what}); nothing was sent")]
    Changed {
        /// `owner/name#number`.
        pull_request: String,
        /// What changed.
        what: String,
    },
    /// Something stands in the way of the merge.
    #[error("merge {pull_request}: {blocker}; nothing was sent")]
    Blocked {
        /// `owner/name#number`.
        pull_request: String,
        /// What.
        blocker: MergeBlocker,
    },
    /// The user did not confirm, or typed the wrong phrase.
    #[error("merge {pull_request}: not confirmed{}; nothing was sent", phrase_hint(.phrase.as_deref()))]
    NotConfirmed {
        /// `owner/name#number`.
        pull_request: String,
        /// The phrase to type, when the challenge names one.
        phrase: Option<String>,
    },
    /// The policy refuses the merge outright.
    #[error("{reason}")]
    Denied {
        /// The policy's message.
        reason: String,
    },
    /// Cancelled before the request left.
    #[error("merge {pull_request}: cancelled before anything was sent")]
    Cancelled {
        /// `owner/name#number`.
        pull_request: String,
    },
    /// The request could not leave the machine.
    #[error("merge {pull_request}: the request could not be sent, so nothing changed: {source}")]
    NotSent {
        /// `owner/name#number`.
        pull_request: String,
        /// Why.
        source: GitHubError,
    },
}

fn phrase_hint(phrase: Option<&str>) -> String {
    phrase.map_or_else(String::new, |phrase| {
        format!("; type '{phrase}' to confirm")
    })
}

/// What the challenge must show for merging the pull request `merge` names: reads it now,
/// refuses when it changed since the stamp or cannot merge as asked, and answers the policy's
/// decision for that read. Sends nothing but the read.
///
/// # Errors
/// [`MergeRefusal::Read`], [`MergeRefusal::Cancelled`], [`MergeRefusal::Changed`],
/// [`MergeRefusal::Blocked`], and [`MergeRefusal::Denied`] when the policy refuses it.
pub fn plan_merge_pull_request(
    policy: &Policy,
    merge: &MergeTarget<'_>,
    cancel: &Cancel,
) -> Result<MergePlan, MergeRefusal> {
    let fresh = fresh(merge, cancel)?;
    let decision = policy.evaluate(&fresh.action);
    if let Decision::Deny { reason } = decision {
        return Err(MergeRefusal::Denied { reason });
    }
    Ok(MergePlan {
        decision,
        detail: fresh.detail,
        bypassed: fresh.bypassed,
    })
}

/// Merges the pull request `request` names, as the module's documentation lays out: a fresh
/// read compared with the stamp, the blockers, the policy's own decision checked against the
/// answered one, then one mutation. A cancel before the request leaves sends nothing; after,
/// it cannot stop the merge, and the outcome is read as for a lost answer.
///
/// # Errors
/// A [`MergeRefusal`] whenever nothing was sent; once the request may have left, the answer is
/// a [`MergeOutcome`], never an error.
pub fn merge_pull_request(
    policy: &Policy,
    request: MergeRequest<'_>,
    cancel: &Cancel,
) -> Result<MergeOutcome, MergeRefusal> {
    let merge = &request.merge;
    let pull_request = label(merge.target, merge.stamp.number);
    let action = fresh(merge, cancel)?.action;
    let proof = confirmed(policy, &action, request.answer, &pull_request)?;
    if cancel.is_cancelled() {
        return Err(MergeRefusal::Cancelled { pull_request });
    }
    (request.on_send)();
    let expected = merge.stamp.head_oid;
    let lost = match Session::new(merge.host, merge.token).mutate(&proof, cancel) {
        Mutated::NotSent(GitHubError::Cancelled { .. }) => {
            return Err(MergeRefusal::Cancelled { pull_request });
        }
        Mutated::NotSent(source) => {
            return Err(MergeRefusal::NotSent {
                pull_request,
                source,
            });
        }
        Mutated::Answered(answer) => match answered(answer, expected) {
            Ok(outcome) => return Ok(outcome),
            Err(problem) => problem,
        },
        Mutated::Lost(error) => error.to_string(),
    };
    Ok(settle(merge, lost, cancel))
}

/// A read made just now and the merge it allows.
struct Fresh {
    detail: PullRequestDetail,
    action: Action,
    bypassed: Option<Bypassed>,
}

/// The pull request read now, refused when it changed since the stamp or cannot merge as
/// asked, and the action that merges it the way [`MergeTarget::kind`] says.
fn fresh(merge: &MergeTarget<'_>, cancel: &Cancel) -> Result<Fresh, MergeRefusal> {
    let pull_request = label(merge.target, merge.stamp.number);
    let detail = read_pull_request(
        merge.host,
        merge.token,
        merge.target,
        merge.stamp.number,
        cancel,
    )
    .map_err(|source| match source {
        GitHubError::Cancelled { .. } => MergeRefusal::Cancelled {
            pull_request: pull_request.clone(),
        },
        source => MergeRefusal::Read {
            pull_request: pull_request.clone(),
            source,
        },
    })?;
    if let Some(what) = changed(merge.stamp, &detail.stamp()) {
        return Err(MergeRefusal::Changed { pull_request, what });
    }
    let blocked = |blocker| MergeRefusal::Blocked {
        pull_request: pull_request.clone(),
        blocker,
    };
    let (action, bypassed) = match merge.kind {
        MergeKind::Plain => {
            let plain =
                PullRequestMerge::new(merge.target, &detail, merge.method).map_err(blocked)?;
            (Action::MergePullRequest(Box::new(plain)), None)
        }
        MergeKind::Admin => {
            let admin =
                PullRequestAdminMerge::new(merge.target, &detail, merge.method).map_err(blocked)?;
            let bypassed = admin.bypassed().clone();
            (
                Action::AdminMergePullRequest(Box::new(admin)),
                Some(bypassed),
            )
        }
    };
    Ok(Fresh {
        detail,
        action,
        bypassed,
    })
}

/// What differs between what the user saw and what was just read, as a phrase; `None` when
/// nothing the stamp holds does.
fn changed(seen: &PullRequestStamp, now: &PullRequestStamp) -> Option<String> {
    let mut changes = Vec::new();
    if now.number != seen.number {
        changes.push(format!(
            "the host answered #{} for #{}",
            now.number, seen.number
        ));
    }
    if now.head_oid != seen.head_oid {
        changes.push(format!(
            "its head is now {}, not {}",
            now.head_oid, seen.head_oid
        ));
    }
    if now.base_ref != seen.base_ref {
        changes.push(format!(
            "its base is now '{}', not '{}'",
            now.base_ref, seen.base_ref
        ));
    }
    if now.state != seen.state {
        changes.push(format!(
            "it is now {}, not {}",
            state_word(now.state),
            state_word(seen.state)
        ));
    }
    (!changes.is_empty()).then(|| changes.join("; "))
}

/// The proof for `action`: the policy's decision now must be the one the user answered, and
/// the answer must verify it. No answer, an answer to another decision, or a wrong phrase gives
/// none.
fn confirmed(
    policy: &Policy,
    action: &Action,
    answer: Option<MergeAnswer<'_>>,
    pull_request: &str,
) -> Result<Confirmation, MergeRefusal> {
    let Some(answer) = answer else {
        return Err(MergeRefusal::NotConfirmed {
            pull_request: pull_request.to_owned(),
            phrase: None,
        });
    };
    let now = policy.evaluate(action);
    if now != *answer.decision {
        let what = match &now {
            Decision::Confirm(challenge) => format!(
                "what you confirmed no longer describes it; now: {}",
                challenge.summary()
            ),
            Decision::Allow | Decision::Deny { .. } => {
                "what you confirmed is not what the policy asks now".to_owned()
            }
        };
        return Err(MergeRefusal::Changed {
            pull_request: pull_request.to_owned(),
            what,
        });
    }
    policy
        .authorize(action, Some(answer.typed))
        .map_err(|refusal| match refusal {
            Refusal::Denied(reason) => MergeRefusal::Denied { reason },
            Refusal::NotConfirmed { phrase } => MergeRefusal::NotConfirmed {
                pull_request: pull_request.to_owned(),
                phrase: Some(phrase).filter(|phrase| !phrase.is_empty()),
            },
        })
}

/// The kinds of GraphQL error that mean GitHub refused the mutation: it ran and said no, or
/// never ran because the request was not one it takes. Anything else (no type, `INTERNAL`, a
/// type this build does not know, GitHub's timeout text) leaves the outcome unknown.
const REFUSING_KINDS: [&str; 4] = ["UNPROCESSABLE", "FORBIDDEN", "NOT_FOUND", "UNAUTHORIZED"];

fn refuses(error: &GraphqlError) -> bool {
    error.code.is_some()
        || error
            .kind
            .as_deref()
            .is_some_and(|kind| REFUSING_KINDS.contains(&kind))
}

/// What the host's answer to the mutation says: a final outcome, or, when it settles nothing,
/// the problem to report beside the read that follows.
fn answered(
    answer: Result<GraphqlReply, GitHubError>,
    expected: Oid,
) -> Result<MergeOutcome, String> {
    let reply = match answer {
        Ok(reply) => reply,
        Err(error @ (GitHubError::Unauthorized { .. } | GitHubError::RateLimited { .. })) => {
            return Ok(MergeOutcome::Refused {
                reason: error.to_string(),
            });
        }
        Err(error @ GitHubError::Status { status, .. }) if (400..500).contains(&status) => {
            return Ok(MergeOutcome::Refused {
                reason: error.to_string(),
            });
        }
        // A 5xx, or a 2xx that is not JSON: the host may have merged before it failed.
        Err(error) => return Err(error.to_string()),
    };
    let merged = reply
        .data
        .as_ref()
        .and_then(|data| data.get("mergePullRequest"))
        .and_then(|payload| payload.get("pullRequest"))
        .filter(|pull_request| !pull_request.is_null());
    if let Some(pull_request) = merged
        && pull_request.get("merged").and_then(Value::as_bool) == Some(true)
        && oid_at(pull_request, &["headRefOid"]) == Some(expected)
    {
        return Ok(MergeOutcome::Merged {
            merge_commit: oid_at(pull_request, &["mergeCommit", "oid"]),
            settled: Settled::Answer,
        });
    }
    match reply.errors.first() {
        Some(first) if reply.errors.iter().all(refuses) => Ok(MergeOutcome::Refused {
            reason: first.message.clone(),
        }),
        Some(first) => Err(format!(
            "GitHub answered with an error that does not say whether it merged: {}",
            first.message
        )),
        None => Err("GitHub's answer did not say the pull request merged".to_owned()),
    }
}

/// The object id at `path` inside `value`.
fn oid_at(value: &Value, path: &[&str]) -> Option<Oid> {
    let mut node = value;
    for key in path {
        node = node.get(key)?;
    }
    Oid::from_hex(node.as_str()?).ok()
}

/// The outcome of a request whose answer was lost: one read of the pull request, never a second
/// mutation. The read watches a fresh flag with the caller's budgets, since a cancel after the
/// request left cannot stop the merge and this read is what tells the user where it stands.
fn settle(merge: &MergeTarget<'_>, problem: String, cancel: &Cancel) -> MergeOutcome {
    let expected = merge.stamp.head_oid;
    let reread = Cancel::new()
        .with_connect_timeout(cancel.connect())
        .with_idle_timeout(cancel.idle());
    let after = match read_pull_request(
        merge.host,
        merge.token,
        merge.target,
        merge.stamp.number,
        &reread,
    ) {
        Err(error) => AfterLoss::Unread {
            reason: error.to_string(),
        },
        Ok(detail) => {
            let head = detail.summary.head_oid;
            match detail.summary.state {
                PullRequestState::Merged if head == expected => {
                    return MergeOutcome::Merged {
                        merge_commit: None,
                        settled: Settled::Reread,
                    };
                }
                PullRequestState::Merged => AfterLoss::MergedAtOtherHead { head },
                PullRequestState::Closed => AfterLoss::Closed,
                PullRequestState::Open => AfterLoss::StillOpen { head },
                PullRequestState::Unknown => AfterLoss::Unread {
                    reason: "the host reported a state rc-git does not know".to_owned(),
                },
            }
        }
    };
    MergeOutcome::Unknown { problem, after }
}
