//! What the pull request actions after the merge share: turning auto-merge on and off
//! ([`super::auto_merge`]), re-running failed jobs ([`super::rerun`]) and deleting a merged pull
//! request's head branch ([`super::delete_branch`], which reads the branch with its own query but
//! checks the stamp, the answered decision and the typed name here). They follow the merge's
//! sequence ([`super::merge`]) and reuse its pieces: the read made just now and compared with
//! the [`PullRequestStamp`] of what the user saw, a challenge of labelled lines whose values are
//! neutralized, the policy's decision now checked against the answered one, and requests sent
//! once each through the proof. Every refusal here means nothing was sent.

use rc_git_core::Oid;

use super::merge::{
    BRANCH_CHARS, LOGIN_CHARS, MergeBlocker, TITLE_CHARS, changed, changed_lines, fact, fact_line,
    label, quoted,
};
use super::pull_types::{PullRequestDetail, PullRequestStamp, PullRequestState};
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
    /// Deleting the head branch of a merged pull request.
    DeleteHeadBranch,
}

impl std::fmt::Display for PullRequestAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::EnableAutoMerge => "turn on auto-merge for",
            Self::DisableAutoMerge => "turn off auto-merge for",
            Self::RerunFailedJobs => "re-run the failed jobs of",
            Self::DeleteHeadBranch => "delete the head branch of",
        })
    }
}

/// Which way an open pull request uses a branch about to be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchUse {
    /// As its head branch: GitHub closes it once the branch is gone.
    Head,
    /// As its base branch: GitHub closes or retargets it once the branch is gone.
    Base,
}

/// `head` or `base`.
fn branch_use_word(branch_use: BranchUse) -> &'static str {
    match branch_use {
        BranchUse::Head => "head",
        BranchUse::Base => "base",
    }
}

/// `#9` and how many more, for a blocker that names open pull requests.
fn open_pull_requests(total: u64, first: Option<u64>, branch_use: BranchUse) -> String {
    let role = branch_use_word(branch_use);
    let then = match branch_use {
        BranchUse::Head => "GitHub would close",
        BranchUse::Base => "GitHub would close or retarget",
    };
    match first {
        Some(first) if total > 1 => format!(
            "open pull request #{first} and {} more also use it as their {role} branch, which \
             {then}",
            total - 1
        ),
        Some(first) => {
            format!("open pull request #{first} also uses it as its {role} branch, which {then}")
        }
        None => format!(
            "{total} open pull request(s) GitHub did not number also use it as their {role} \
             branch, which {then}"
        ),
    }
}

/// The first seven hex digits of `oid`, or `nothing` when there is none.
fn short_or_nothing(oid: Option<Oid>) -> String {
    oid.map_or_else(
        || "no commit".to_owned(),
        |oid| oid.to_hex().chars().take(7).collect(),
    )
}

/// The first seven hex digits of `oid`.
fn short(oid: Oid) -> String {
    oid.to_hex().chars().take(7).collect()
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
    /// The pull request is not merged: rc-git deletes only a merged pull request's head branch.
    #[error("{}", not_merged(*.0))]
    NotMerged(PullRequestState),
    /// The head branch is in another repository than the base (a fork), or GitHub does not name
    /// it as this repository's: rc-git deletes only a branch of the base repository.
    #[error("its head branch is not a branch of this repository; rc-git deletes no fork's branch")]
    HeadNotInRepository,
    /// GitHub does not say the account may delete the head branch (`viewerCanDeleteHeadRef`).
    #[error("GitHub does not report that your account may delete its head branch")]
    NoDeleteRight,
    /// The head branch no longer exists on GitHub.
    #[error("its head branch no longer exists on GitHub")]
    BranchGone,
    /// The head branch points at another commit than the head the pull request merged: someone
    /// pushed to it, or made it again, after the merge.
    #[error(
        "its head branch is at {}, not at {}, the head the pull request merged: it holds commits \
         the pull request did not merge",
        short_or_nothing(*.now),
        short(*.merged)
    )]
    BranchMoved {
        /// Where the branch is now, when GitHub named a commit.
        now: Option<Oid>,
        /// The head the pull request merged.
        merged: Oid,
    },
    /// The name holds a character a challenge shows as an escape (a control, bidi or invisible
    /// character, a double quote character, whitespace) or is too long to show whole, so it
    /// cannot be typed exactly as its challenge shows it.
    #[error(
        "its name holds characters a confirmation shows as escapes, or is too long to show, so it \
         cannot be typed exactly; delete it on GitHub's own page or with git"
    )]
    NameNotTypeable,
    /// It is the repository's default branch.
    #[error("it is the repository's default branch")]
    DefaultBranch,
    /// GitHub did not name the repository's default branch, so rc-git cannot tell this is not it.
    #[error("GitHub did not name the repository's default branch, so rc-git cannot rule it out")]
    DefaultBranchUnreported,
    /// Other open pull requests use the branch, which deleting it would close or retarget.
    #[error("{}", open_pull_requests(*.total, *.first, *.branch_use))]
    OpenPullRequests {
        /// How they use it.
        branch_use: BranchUse,
        /// How many GitHub counts.
        total: u64,
        /// The first one's number, when GitHub named one.
        first: Option<u64>,
    },
    /// GitHub's answer did not count the open pull requests that use the branch this way.
    #[error(
        "GitHub did not say which open pull requests use it as their {} branch",
        branch_use_word(*.0)
    )]
    OpenPullRequestsUnread(BranchUse),
    /// The host sent no node id for the repository, so no delete can name it.
    #[error("the host sent no id for the repository, so rc-git cannot name it in a delete")]
    NoRepositoryId,
    /// The head commit is not in the local repository, so no backup ref can keep it.
    #[error(
        "its head commit {} is not in this repository, so no backup ref can keep it; fetch the \
         pull request's changes first (Fetch Changes on its page, or rc-git pr diff --fetch)",
        short(*.0)
    )]
    NotFetched(Oid),
}

/// Why a pull request in `state` has no head branch to delete.
fn not_merged(state: PullRequestState) -> &'static str {
    match state {
        PullRequestState::Open => "it is open, not merged",
        PullRequestState::Closed => "it was closed without merging",
        PullRequestState::Merged => "GitHub does not report it merged",
        PullRequestState::Unknown => "it is in a state rc-git does not know, not merged",
    }
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
    /// Something stands in the way of deleting the head branch, which the refusal names.
    #[error("delete branch {branch} of {pull_request}: {blocker}; nothing was sent or written")]
    BranchBlocked {
        /// `owner/name#number`.
        pull_request: String,
        /// The branch, neutralized and in curly quotes.
        branch: String,
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
    /// The challenge names a phrase and the user typed something else, or nothing.
    #[error(
        "{action} {pull_request}: not confirmed; type \u{201c}{phrase}\u{201d} exactly to confirm; \
         nothing was sent"
    )]
    NotTyped {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
        /// The phrase the challenge asks for: text a challenge shows unchanged.
        phrase: String,
    },
    /// The local repository could not be read, or the backup ref could not be written or read
    /// back, before anything was sent.
    #[error("{action} {pull_request}: {reason}; nothing was sent")]
    Local {
        /// Which action.
        action: PullRequestAction,
        /// `owner/name#number`.
        pull_request: String,
        /// What failed, naming the repository or the ref.
        reason: String,
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
    /// A head branch delete wrote its backup ref, then stopped before its request left: the
    /// branch on GitHub is untouched, and the backup ref stays.
    #[error(
        "delete branch {branch} of {pull_request}: {stop}, so nothing was sent and the branch on \
         GitHub is untouched; the backup ref {backup_ref} written first stays, so delete it with \
         git when you no longer need it"
    )]
    BackupKept {
        /// `owner/name#number`.
        pull_request: String,
        /// The branch, in curly quotes.
        branch: String,
        /// The backup ref written.
        backup_ref: String,
        /// Why the request did not leave.
        stop: BackupKeptStop,
    },
}

/// Why a head branch delete stopped after its backup ref was written and before its request left.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupKeptStop {
    /// Cancelled before the request left.
    #[error("cancelled before the request left")]
    Cancelled,
    /// The request could not leave the machine.
    #[error("the request could not be sent ({0})")]
    NotSent(Box<GitHubError>),
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

/// What differs between the stamp `seen` and the read `now` that `action` cares about: every
/// field of the stamp, except that turning auto-merge off, the safe direction, compares the number
/// and the state alone, so a push or a retarget between the challenge and the confirm never keeps
/// auto-merge on.
fn changed_for(
    action: PullRequestAction,
    seen: &PullRequestStamp,
    now: &PullRequestStamp,
) -> Option<String> {
    match action {
        PullRequestAction::DisableAutoMerge => {
            let compared = PullRequestStamp {
                head_oid: seen.head_oid,
                base_ref: seen.base_ref.clone(),
                ..now.clone()
            };
            changed(seen, &compared)
        }
        PullRequestAction::EnableAutoMerge
        | PullRequestAction::RerunFailedJobs
        | PullRequestAction::DeleteHeadBranch => changed(seen, now),
    }
}

/// The pull request `who` names, read now under `cancel`, refused when it changed since the
/// stamp as far as `action` compares it ([`changed_for`]). Sends nothing but the read.
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
    if let Some(what) = changed_for(action, who.stamp, &detail.stamp()) {
        return Err(ActionRefusal::Changed {
            action,
            pull_request,
            what,
        });
    }
    Ok(detail)
}

/// The proof for `built`, the action rc-git built from the read made just now: the policy's
/// decision for it now must be the one the user `answered`, and `typed`, what the user typed
/// untouched (empty for a plain confirm), must verify it. No answer, an answer to another
/// decision, or a phrase typed wrong gives none.
pub(super) fn confirmed(
    policy: &Policy,
    built: &Action,
    (answered, typed): (Option<&Decision>, &str),
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
        .authorize(built, Some(typed))
        .map_err(|refusal| match refusal {
            Refusal::Denied(reason) => ActionRefusal::Denied { reason },
            Refusal::NotConfirmed { phrase } if !phrase.is_empty() => ActionRefusal::NotTyped {
                action,
                pull_request,
                phrase,
            },
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
    let proof = confirmed(policy, &built, (answered, ""), (action, &pull_request))?;
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
