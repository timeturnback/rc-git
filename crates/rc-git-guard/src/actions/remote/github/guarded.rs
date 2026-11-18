//! What the pull request actions after the merge share: turning auto-merge on and off
//! ([`super::auto_merge`]) and re-running failed jobs ([`super::rerun`]). They follow the merge's
//! sequence ([`super::merge`]) and reuse its pieces: the read made just now and compared with
//! the [`PullRequestStamp`] of what the user saw, a challenge of labelled lines whose values are
//! neutralized, the policy's decision now checked against the answered one, and requests sent
//! once each through the proof. Every refusal here means nothing was sent.

use rc_git_core::Oid;

use super::merge::{
    BRANCH_CHARS, LOGIN_CHARS, MergeBlocker, TITLE_CHARS, changed, changed_lines, fact, fact_line,
    label, quoted,
};
use super::pull_types::{PullRequestDetail, PullRequestStamp};
use super::pulls::read_pull_request;
use super::secret::ApiToken;
use super::{GitHubError, GitHubHost, RepositoryTarget};
use crate::actions::remote::Cancel;
use crate::{Action, Confirmation, Decision, Policy, Refusal};

/// Which pull request action a refusal is about, as its message names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullRequestAction {
    /// Turning auto-merge on.
    EnableAutoMerge,
    /// Turning auto-merge off.
    DisableAutoMerge,
    /// Re-running the failed jobs of the head's GitHub Actions runs.
    RerunFailedJobs,
}

impl std::fmt::Display for PullRequestAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EnableAutoMerge => "turn on auto-merge for",
            Self::DisableAutoMerge => "turn off auto-merge for",
            Self::RerunFailedJobs => "re-run the failed jobs of",
        })
    }
}

/// What stands between a pull request and an auto-merge change or a re-run, as rc-git reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ActionBlocker {
    /// What the merge would refuse for the same reason: not open, a draft, a method the
    /// repository does not allow, conflicts, a state GitHub has not worked out, no node id.
    #[error("{0}")]
    Merge(MergeBlocker),
    /// Nothing stands in the way of a merge now, so there is nothing for auto-merge to wait for.
    #[error("nothing stands in its way, so it can merge now; use Merge instead")]
    CanMergeNow,
    /// Auto-merge is already on.
    #[error("auto-merge is already on")]
    AutoMergeOn,
    /// Auto-merge is not on.
    #[error("auto-merge is not on")]
    AutoMergeOff,
    /// The repository does not allow auto-merge.
    #[error("the repository does not allow auto-merge")]
    AutoMergeNotAllowed,
    /// The host does not say whether the repository allows auto-merge (an older GitHub
    /// Enterprise Server).
    #[error("this host does not report whether the repository allows auto-merge")]
    AutoMergeUnreported,
    /// GitHub does not say the account may turn auto-merge on (`viewerCanEnableAutoMerge`).
    #[error("GitHub does not report that your account may turn on auto-merge for it")]
    NoEnableRight,
    /// GitHub does not say the account may turn auto-merge off (`viewerCanDisableAutoMerge`).
    #[error("GitHub does not report that your account may turn off its auto-merge")]
    NoDisableRight,
    /// No GitHub Actions run of the head commit has a failed job rc-git can name.
    #[error("no GitHub Actions run of its head commit has a failed job")]
    NoFailedRuns,
}

/// Why a pull request action sent nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionRefusal {
    /// The read before the action failed.
    #[error("{action} {pull_request}: nothing was sent, because reading it first failed: {source}")]
    Read {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
        /// The read's failure.
        source: GitHubError,
    },
    /// It changed since the user looked.
    #[error("{action} {pull_request}: it changed since you looked ({what}); nothing was sent")]
    Changed {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
        /// What changed.
        what: String,
    },
    /// Something stands in the way of the action.
    #[error("{action} {pull_request}: {blocker}; nothing was sent")]
    Blocked {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
        /// What.
        blocker: ActionBlocker,
    },
    /// The user did not confirm.
    #[error("{action} {pull_request}: not confirmed; nothing was sent")]
    NotConfirmed {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
    },
    /// The policy refuses the action outright.
    #[error("{reason}")]
    Denied {
        /// The policy's message.
        reason: String,
    },
    /// Cancelled before any request left.
    #[error("{action} {pull_request}: cancelled before anything was sent")]
    Cancelled {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
    },
    /// The first request could not leave the machine.
    #[error("{action} {pull_request}: the request could not be sent, so nothing changed: {source}")]
    NotSent {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
        /// Why.
        source: GitHubError,
    },
}

/// Which pull request an action is for, with the account that acts.
#[derive(Debug, Clone, Copy)]
pub struct ActionTarget<'a> {
    /// The account's host: the only one the token and the requests go to.
    pub host: &'a GitHubHost,
    /// The account's token.
    pub token: &'a ApiToken,
    /// The account's login, from its record in Settings: the challenge names it.
    pub login: &'a str,
    /// The repository, which must be on `host`.
    pub target: &'a RepositoryTarget,
    /// What the user saw of the pull request.
    pub stamp: &'a PullRequestStamp,
}

impl ActionTarget<'_> {
    /// `owner/name#number` of the pull request the stamp names.
    pub(super) fn pull_request(&self) -> String {
        label(self.target, self.stamp.number)
    }
}

/// What an action needs the user to answer, from a read made just now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPlan {
    /// The policy's decision for the action as read: what the challenge shows.
    pub decision: Decision,
    /// The pull request as read.
    pub detail: PullRequestDetail,
}

/// Where the head branch of a pull request lives when it is not a repository of the base's
/// owner: whoever owns it can push to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Fork {
    /// Owned by this login, neutralized.
    Owner(String),
    /// GitHub did not name the owner (the repository may be gone).
    Unnamed,
}

impl Fork {
    /// The fork of `detail`, read from `target`: a head in another repository whose owner is
    /// not `target`'s owner (logins compare without ASCII case, as GitHub's do).
    fn of(target: &RepositoryTarget, detail: &PullRequestDetail) -> Option<Self> {
        let summary = &detail.summary;
        if !summary.cross_repository {
            return None;
        }
        match &summary.head_owner {
            Some(owner) if owner.eq_ignore_ascii_case(target.owner()) => None,
            Some(owner) => Some(Self::Owner(fact(owner, LOGIN_CHARS))),
            None => Some(Self::Unnamed),
        }
    }

    /// Who owns the head branch's repository, as a challenge names it.
    pub(super) fn owner_phrase(&self) -> String {
        match self {
            Self::Owner(owner) => format!("a repository owned by {}", quoted(owner)),
            Self::Unnamed => "another repository, whose owner GitHub did not name".to_owned(),
        }
    }
}

/// The facts every pull request action's challenge names first, read from the host just before:
/// the repository, the number, the account, the base, the head commit and branch, and the
/// title, which a challenge puts last. Every value is a challenge value ([`fact`]): no line
/// break, double quote, invisible character or space but U+0020.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PullRequestFacts {
    target: RepositoryTarget,
    /// `login@host` of the account that acts, neutralized.
    account: String,
    number: u64,
    title: String,
    base: String,
    head: String,
    head_oid: Oid,
    fork: Option<Fork>,
}

impl PullRequestFacts {
    /// The facts of `detail` as read from `target` with the token of account `login`.
    pub(super) fn read(target: &RepositoryTarget, login: &str, detail: &PullRequestDetail) -> Self {
        let summary = &detail.summary;
        Self {
            target: target.clone(),
            account: format!("{}@{}", fact(login, LOGIN_CHARS), target.host()),
            number: summary.number,
            title: fact(&summary.title, TITLE_CHARS),
            base: fact(&summary.base_ref, BRANCH_CHARS),
            head: fact(&summary.head_ref, BRANCH_CHARS),
            head_oid: summary.head_oid,
            fork: Fork::of(target, detail),
        }
    }

    pub(super) fn target(&self) -> &RepositoryTarget {
        &self.target
    }

    pub(super) fn number(&self) -> u64 {
        self.number
    }

    pub(super) fn head_oid(&self) -> Oid {
        self.head_oid
    }

    pub(super) fn fork(&self) -> Option<&Fork> {
        self.fork.as_ref()
    }

    /// `owner/name#number`.
    pub(super) fn label(&self) -> String {
        label(&self.target, self.number)
    }

    /// The labelled lines from the repository to the head, in the merge challenge's order.
    pub(super) fn lines(&self) -> Vec<String> {
        let short: String = self.head_oid.to_hex().chars().take(7).collect();
        let repository = format!(
            "{}/{} on {}",
            self.target.owner(),
            self.target.name(),
            self.target.host()
        );
        vec![
            fact_line("Repository", &repository),
            fact_line("Pull request", &format!("#{}", self.number)),
            fact_line("Account", &self.account),
            fact_line("Base", &quoted(&self.base)),
            fact_line("Head", &format!("{short} on {}", quoted(&self.head))),
        ]
    }

    /// The title's line, which a challenge puts last.
    pub(super) fn title_line(&self) -> String {
        fact_line("Title", &quoted(&self.title))
    }
}

/// The pull request `who` names, read now under `cancel`, refused when it changed since the
/// stamp. Sends nothing but the read.
pub(super) fn fresh_read(
    who: &ActionTarget<'_>,
    action: PullRequestAction,
    cancel: &Cancel,
) -> Result<PullRequestDetail, ActionRefusal> {
    let pull_request = who.pull_request();
    let detail = read_pull_request(who.host, who.token, who.target, who.stamp.number, cancel)
        .map_err(|source| match source {
            GitHubError::Cancelled { .. } => ActionRefusal::Cancelled {
                action,
                pull_request: pull_request.clone(),
            },
            source => ActionRefusal::Read {
                action,
                pull_request: pull_request.clone(),
                source,
            },
        })?;
    if let Some(what) = changed(who.stamp, &detail.stamp()) {
        return Err(ActionRefusal::Changed {
            action,
            pull_request,
            what,
        });
    }
    Ok(detail)
}

/// The proof for `built`, the action rc-git built from the read made just now: the policy's
/// decision for it now must be the one the user `answered`, and the answer must verify it. No
/// answer, or an answer to another decision, gives none.
pub(super) fn confirmed(
    policy: &Policy,
    built: &Action,
    answered: Option<&Decision>,
    (action, pull_request): (PullRequestAction, &str),
) -> Result<Confirmation, ActionRefusal> {
    let pull_request = pull_request.to_owned();
    let Some(answered) = answered else {
        return Err(ActionRefusal::NotConfirmed {
            action,
            pull_request,
        });
    };
    let now = policy.evaluate(built);
    if now != *answered {
        let what = match &now {
            Decision::Confirm(challenge) => format!(
                "what you confirmed no longer describes it; now: {}",
                changed_lines(answered, challenge.summary())
            ),
            Decision::Allow | Decision::Deny { .. } => {
                "what you confirmed is not what the policy asks now".to_owned()
            }
        };
        return Err(ActionRefusal::Changed {
            action,
            pull_request,
            what,
        });
    }
    policy
        .authorize(built, Some(""))
        .map_err(|refusal| match refusal {
            Refusal::Denied(reason) => ActionRefusal::Denied { reason },
            Refusal::NotConfirmed { .. } => ActionRefusal::NotConfirmed {
                action,
                pull_request,
            },
        })
}

/// The plan for `action`: the read made just now, refused when it changed since the stamp, and
/// the policy's decision for the action `build` makes of it. Sends nothing but the read.
pub(super) fn plan_action(
    policy: &Policy,
    who: &ActionTarget<'_>,
    action: PullRequestAction,
    build: impl FnOnce(&PullRequestDetail) -> Result<Action, ActionBlocker>,
    cancel: &Cancel,
) -> Result<ActionPlan, ActionRefusal> {
    let detail = fresh_read(who, action, cancel)?;
    let built = build(&detail).map_err(|blocker| ActionRefusal::Blocked {
        action,
        pull_request: who.pull_request(),
        blocker,
    })?;
    let decision = policy.evaluate(&built);
    if let Decision::Deny { reason } = decision {
        return Err(ActionRefusal::Denied { reason });
    }
    Ok(ActionPlan { decision, detail })
}

/// The read made just now and the proof for the action `build` makes of it, ready to send:
/// refused when the pull request changed since the stamp, when something stands in the way,
/// when the answered decision is not the policy's own now, and when the caller's cancel is up.
pub(super) fn proof_for(
    policy: &Policy,
    who: &ActionTarget<'_>,
    (action, answered): (PullRequestAction, Option<&Decision>),
    build: impl FnOnce(&PullRequestDetail) -> Result<Action, ActionBlocker>,
    cancel: &Cancel,
) -> Result<Confirmation, ActionRefusal> {
    let pull_request = who.pull_request();
    let detail = fresh_read(who, action, cancel)?;
    let built = build(&detail).map_err(|blocker| ActionRefusal::Blocked {
        action,
        pull_request: pull_request.clone(),
        blocker,
    })?;
    let proof = confirmed(policy, &built, answered, (action, &pull_request))?;
    if cancel.is_cancelled() {
        return Err(ActionRefusal::Cancelled {
            action,
            pull_request,
        });
    }
    Ok(proof)
}

/// A cancel flag with `cancel`'s budgets that only the read itself can raise: the read after a
/// request whose answer was lost tells the user where things stand, so a cancel raised after the
/// request left does not stop it.
pub(super) fn settling(cancel: &Cancel) -> Cancel {
    Cancel::new()
        .with_connect_timeout(cancel.connect())
        .with_idle_timeout(cancel.idle())
}
