//! Deleting a merged pull request's head branch on GitHub. It deletes a branch everyone who uses
//! the repository shares, so it is a remote branch delete: the policy decides it with the one
//! rule [`Action::DeleteRemoteBranch`] has (denied on a protected branch; elsewhere the branch's
//! name typed and a backup ref), and nothing is sent until a backup ref in the local repository
//! holds the branch's commit.
//!
//! The sequence, inside [`delete_head_branch`] (the app and the CLI both call it):
//!
//! 1. **Read it now.** One GraphQL read ([`HEAD_BRANCH`]) of the pull request and its head
//!    branch: state, head, base, the head branch's ref (node id, name, prefix, commit,
//!    repository), the open pull requests that have it as their head, whether the account may
//!    delete it, and the repository's default branch. It is compared with the
//!    [`PullRequestStamp`] of what the user saw; any difference refuses.
//! 2. **Refuse what is not a merged pull request's own branch** ([`delete_branch_blocker`]): a
//!    pull request that is not merged; a head in a fork or any repository but the base; an account
//!    GitHub does not say may delete it; a branch that is gone, is not the head branch, or points
//!    at another commit than the head the pull request merged (someone pushed to it after the
//!    merge); a name a challenge cannot show unchanged; the default branch; another open pull
//!    request with it as head. A second read ([`BASED_ON`]) refuses any open pull request based
//!    on it, which GitHub would close or retarget.
//! 3. **The way back must exist first.** The head commit must be in the local object database,
//!    so a backup ref can hold it; otherwise the refusal says to fetch the pull request's changes
//!    first. Nothing here fetches.
//! 4. **Authorize** the action built from that read with the typed name: the decision the user
//!    answered must be the policy's own now.
//! 5. **Back up, then send.** [`Writer::write_backup_ref`] writes
//!    `refs/rc-git/backup/remote-branch/<name>` at the head commit, and the ref is read back at
//!    that commit; a failure sends nothing. Only then does the one `deleteRef` mutation leave,
//!    naming the ref by its node id. It is never sent again.
//! 6. **Three outcomes.** Deleted, refused, or unknown; only a lost answer reads the head branch
//!    once more: gone is deleted, still there at the same commit is not deleted, anything else
//!    stays unknown. The backup ref is kept whatever the outcome.
//!
//! Every refusal before step 5 is an [`ActionRefusal`] and means nothing was sent or written.

use std::path::Path;

use rc_git_core::Oid;
use serde_json::{Value, json};

use super::api::{GraphqlReply, Mutated, Session};
use super::guarded::{
    ActionBlocker, ActionRefusal, ActionTarget, BranchUse, PullRequestAction, confirmed, settling,
};
use super::merge::{
    BRANCH_CHARS, FirstLook, LOGIN_CHARS, Settled, TITLE_CHARS, changed, errors_verdict, fact,
    fact_line, first_look, label, quoted,
};
use super::pull_types::{PullRequestStamp, PullRequestState, RefId};
use super::pulls::{MAX_LINE_CHARS, flag, nodes, pull_request_state, repository, text};
use super::text::single_line;
use super::{GitHubError, GitHubResult, RepositoryTarget};
use crate::actions::remote::Cancel;
use crate::actions::{Writer, backup_base, open};
use crate::{Action, Confirmation, Decision, Policy};

/// The kind a head branch's backup ref is filed under: `refs/rc-git/backup/remote-branch/<name>`.
pub const BACKUP_KIND: &str = "remote-branch";

/// How many open pull requests a read names; the rest are counted.
const NAMED_PULL_REQUESTS: u32 = 5;

/// The pull request and its head branch, with every fact a delete is judged on. Checked against
/// GitHub's GraphQL reference: `headRef` (`Ref`, null once the branch is gone), whose `id`,
/// `name`, `prefix` ("such as refs/heads/"), `target` (`GitObject`, its `oid`), `repository` and
/// `associatedPullRequests` ("pull requests with this ref as the head ref", filtered by `states`)
/// are its fields; the pull request's `merged`, `isCrossRepository`, `headRepository` and
/// `viewerCanDeleteHeadRef`; and the repository's `defaultBranchRef`.
pub(super) const HEAD_BRANCH: &str = "query RcGitHeadBranch($owner: String!, $name: String!, \
$number: Int!, $first: Int!) { repository(owner: $owner, name: $name) { \
defaultBranchRef { name } pullRequest(number: $number) { number title state merged \
headRefName headRefOid baseRefName isCrossRepository viewerCanDeleteHeadRef \
headRepository { name owner { login } } headRef { id name prefix target { oid } \
repository { name owner { login } } associatedPullRequests(states: [OPEN], first: $first) { \
totalCount nodes { number } } } } } }";

/// The open pull requests based on a branch: `pullRequests` filtered by `baseRefName` and
/// `states`, both arguments of the field in GitHub's GraphQL reference.
pub(super) const BASED_ON: &str = "query RcGitBranchBase($owner: String!, $name: String!, \
$branch: String!, $first: Int!) { repository(owner: $owner, name: $name) { \
pullRequests(states: [OPEN], baseRefName: $branch, first: $first) { totalCount \
nodes { number } } } }";

/// The variables of [`HEAD_BRANCH`] for pull request `number` of `target`.
pub(super) fn head_branch_variables(target: &RepositoryTarget, number: u64) -> Value {
    json!({
        "owner": target.owner(),
        "name": target.name(),
        "number": number,
        "first": NAMED_PULL_REQUESTS,
    })
}

/// The variables of [`BASED_ON`] for `branch` of `target`.
pub(super) fn based_on_variables(target: &RepositoryTarget, branch: &str) -> Value {
    json!({
        "owner": target.owner(),
        "name": target.name(),
        "branch": branch,
        "first": NAMED_PULL_REQUESTS,
    })
}

/// A head branch's name as GitHub reported it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BranchName {
    /// The name exactly as GitHub sent it, which a challenge shows unchanged ([`typeable`]): the
    /// phrase the user types.
    Exact(String),
    /// A name a challenge would show changed (an escape, a double quote character, whitespace,
    /// cut short): only its neutralized form, which is never typed.
    Shown(String),
}

impl BranchName {
    /// `raw`, the name as the server sent it: [`BranchName::Exact`] when [`typeable`], else its
    /// neutralized form.
    #[must_use]
    pub fn from_server(raw: &str) -> Self {
        if typeable(raw) {
            Self::Exact(raw.to_owned())
        } else {
            Self::Shown(fact(raw, BRANCH_CHARS))
        }
    }

    /// The name as a challenge shows it.
    #[must_use]
    pub fn shown(&self) -> &str {
        match self {
            Self::Exact(name) | Self::Shown(name) => name,
        }
    }

    /// The exact name, when it can be typed.
    #[must_use]
    pub fn exact(&self) -> Option<&str> {
        match self {
            Self::Exact(name) if typeable(name) => Some(name),
            Self::Exact(_) | Self::Shown(_) => None,
        }
    }
}

/// Whether `name` reads the same in a challenge as it is, so typing what the challenge shows
/// types the name: not empty, no whitespace and no backslash (git allows neither in a branch
/// name, and a backslash reads like the start of an escape), and every character shown as itself
/// and the whole of it shown, as a challenge's values are.
#[must_use]
pub fn typeable(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && !name.contains('\\')
        && fact(name, BRANCH_CHARS) == name
}

/// How many open pull requests use a branch, and the first one's number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OpenPullRequests {
    /// How many GitHub counts.
    pub total: u64,
    /// The first one's number, when GitHub named one.
    pub first: Option<u64>,
}

/// The pull request's head branch as GitHub's `headRef` reported it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HeadRef {
    /// Its node id, the one thing the mutation names it by.
    pub id: Option<RefId>,
    /// The commit it points at.
    pub target: Option<Oid>,
    /// Whether it is `refs/heads/<headRefName>` of the base repository: its prefix, its name and
    /// its repository all as the pull request names them.
    pub is_head_branch: bool,
    /// The open pull requests that have it as their head; `None` when GitHub did not say.
    pub open_heads: Option<OpenPullRequests>,
}

/// One read of a pull request's head branch, holding no server text but neutralized text and
/// what rc-git judged of the rest: everything a delete is decided on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HeadBranch {
    /// The pull request's number, head commit, base (single-line) and state.
    pub stamp: PullRequestStamp,
    /// GitHub's `merged`.
    pub merged: bool,
    /// The title, single-line.
    pub title: String,
    /// The head branch's name.
    pub branch: BranchName,
    /// Whether the head is in the base repository: not cross-repository, and the head
    /// repository is the target's (owner and name compared without ASCII case).
    pub in_repository: bool,
    /// GitHub's `viewerCanDeleteHeadRef`.
    pub viewer_can_delete: bool,
    /// The head branch; `None` when it is gone.
    pub head_ref: Option<HeadRef>,
    /// Whether the head branch is the repository's default branch (compared without ASCII
    /// case); `None` when GitHub named no default branch.
    pub default_branch: Option<bool>,
    /// The open pull requests based on the head branch; `None` until that second read, or when
    /// GitHub did not say.
    pub open_bases: Option<OpenPullRequests>,
}

/// What stands in the way of deleting `read`'s head branch; `None` when nothing does. The one
/// rule the delete itself follows; the control decides from the pull request's detail
/// ([`head_branch_blocker`]) and the delete reads again.
#[must_use]
pub fn delete_branch_blocker(read: &HeadBranch) -> Option<ActionBlocker> {
    if let Some(blocker) = branch_blocker(read) {
        return Some(blocker);
    }
    open_blocker(read.open_bases, BranchUse::Base)
}

/// [`delete_branch_blocker`] without the open pull requests based on the branch, which a second
/// read finds.
fn branch_blocker(read: &HeadBranch) -> Option<ActionBlocker> {
    if read.stamp.state != PullRequestState::Merged || !read.merged {
        return Some(ActionBlocker::NotMerged(read.stamp.state));
    }
    if !read.in_repository {
        return Some(ActionBlocker::HeadNotInRepository);
    }
    if !read.viewer_can_delete {
        return Some(ActionBlocker::NoDeleteRight);
    }
    let Some(head_ref) = &read.head_ref else {
        return Some(ActionBlocker::BranchGone);
    };
    if !head_ref.is_head_branch {
        return Some(ActionBlocker::HeadNotInRepository);
    }
    if head_ref.target != Some(read.stamp.head_oid) {
        return Some(ActionBlocker::BranchMoved {
            now: head_ref.target,
            merged: read.stamp.head_oid,
        });
    }
    if read.branch.exact().is_none() {
        return Some(ActionBlocker::NameNotTypeable);
    }
    match read.default_branch {
        Some(false) => {}
        Some(true) => return Some(ActionBlocker::DefaultBranch),
        None => return Some(ActionBlocker::DefaultBranchUnreported),
    }
    if let Some(blocker) = open_blocker(head_ref.open_heads, BranchUse::Head) {
        return Some(blocker);
    }
    head_ref.id.is_none().then_some(ActionBlocker::NoRefId)
}

/// What open pull requests using the branch as `branch_use` stand for: none is nothing, any is a
/// refusal naming one, and an answer GitHub did not give refuses too.
fn open_blocker(open: Option<OpenPullRequests>, branch_use: BranchUse) -> Option<ActionBlocker> {
    match open {
        None => Some(ActionBlocker::OpenPullRequestsUnread(branch_use)),
        Some(OpenPullRequests { total: 0, .. }) => None,
        Some(OpenPullRequests { total, first }) => Some(ActionBlocker::OpenPullRequests {
            branch_use,
            total,
            first,
        }),
    }
}

/// What stands in the way of offering to delete `detail`'s head branch, from what the pull
/// request's detail read holds: not merged, a head in another repository, no right GitHub
/// reports, a name a challenge cannot show unchanged. The delete reads the branch itself again
/// and decides on that ([`delete_branch_blocker`]); this only keeps the control from offering
/// what the delete would refuse for these reasons.
#[must_use]
pub fn head_branch_blocker(detail: &super::PullRequestDetail) -> Option<ActionBlocker> {
    let summary = &detail.summary;
    if summary.state != PullRequestState::Merged {
        return Some(ActionBlocker::NotMerged(summary.state));
    }
    if summary.cross_repository {
        return Some(ActionBlocker::HeadNotInRepository);
    }
    if !detail
        .viewer
        .is_some_and(|viewer| viewer.can_delete_head_ref)
    {
        return Some(ActionBlocker::NoDeleteRight);
    }
    (!typeable(&summary.head_ref)).then_some(ActionBlocker::NameNotTypeable)
}

/// Deleting a merged pull request's head branch as the user confirms it: every fact its
/// challenge names, read from the host just before, and the ref's node id the mutation names it
/// by. Built only by [`PullRequestBranchDelete::new`], from a read nothing in
/// [`delete_branch_blocker`] stands in the way of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestBranchDelete {
    target: RepositoryTarget,
    /// `login@host` of the account that deletes it, neutralized.
    account: String,
    number: u64,
    title: String,
    /// The exact name, which a challenge shows unchanged ([`typeable`]).
    branch: String,
    head_oid: Oid,
    ref_id: RefId,
}

impl PullRequestBranchDelete {
    /// Deleting `read`'s head branch, as read from `target` with the token of account `login`.
    ///
    /// # Errors
    /// The [`ActionBlocker`] [`delete_branch_blocker`] finds.
    pub fn new(
        target: &RepositoryTarget,
        login: &str,
        read: &HeadBranch,
    ) -> Result<Self, ActionBlocker> {
        if let Some(blocker) = delete_branch_blocker(read) {
            return Err(blocker);
        }
        let branch = read.branch.exact().ok_or(ActionBlocker::NameNotTypeable)?;
        let ref_id = read
            .head_ref
            .as_ref()
            .and_then(|head_ref| head_ref.id.clone())
            .ok_or(ActionBlocker::NoRefId)?;
        Ok(Self {
            target: target.clone(),
            account: format!("{}@{}", fact(login, LOGIN_CHARS), target.host()),
            number: read.stamp.number,
            title: fact(&read.title, TITLE_CHARS),
            branch: branch.to_owned(),
            head_oid: read.stamp.head_oid,
            ref_id,
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

    /// The branch's exact name: the phrase its challenge asks for.
    #[must_use]
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// The commit the branch is at, which the backup ref keeps.
    #[must_use]
    pub fn head_oid(&self) -> Oid {
        self.head_oid
    }

    /// The first backup slot for the branch: `refs/rc-git/backup/remote-branch/<name>`.
    #[must_use]
    pub fn backup_ref(&self) -> String {
        backup_base(BACKUP_KIND, &self.branch)
    }

    /// `owner/name#number`.
    #[must_use]
    pub fn label(&self) -> String {
        label(&self.target, self.number)
    }

    pub(super) fn ref_id(&self) -> &RefId {
        &self.ref_id
    }

    /// The challenge's text, one labelled line per fact in the merge's order: rc-git's own first
    /// line, the repository, the pull request, the account, the branch, its commit, the backup
    /// ref, what the delete does for everyone, and the title last.
    pub(crate) fn summary(&self) -> String {
        let short: String = self.head_oid.to_hex().chars().take(7).collect();
        let repository = format!(
            "{}/{} on {}",
            self.target.owner(),
            self.target.name(),
            self.target.host()
        );
        [
            "Delete this pull request's head branch on GitHub. A backup ref in this repository \
             keeps its commit first."
                .to_owned(),
            fact_line("Repository", &repository),
            fact_line("Pull request", &format!("#{}", self.number)),
            fact_line("Account", &self.account),
            fact_line("Branch", &quoted(&self.branch)),
            fact_line("Commit", &short),
            fact_line(
                "Backup",
                &format!(
                    "{} (or the next numbered slot after it), at {short}, written before \
                     anything is sent",
                    self.backup_ref()
                ),
            ),
            fact_line(
                "For everyone",
                "GitHub deletes the branch for everyone who uses the repository; rc-git cannot \
                 put it back by itself, but the backup ref can",
            ),
            fact_line("Title", &quoted(&self.title)),
        ]
        .join("\n")
    }
}

/// What a delete needs the user to answer, from a read made just now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDeletePlan {
    /// The policy's decision: what the challenge shows.
    pub decision: Decision,
    /// The branch's exact name.
    pub branch: String,
    /// The commit it is at.
    pub head_oid: Oid,
    /// The first backup slot the delete writes.
    pub backup_ref: String,
}

/// A delete to run.
#[derive(Clone, Copy)]
pub struct BranchDeleteRequest<'a> {
    /// Which pull request, by which account.
    pub who: ActionTarget<'a>,
    /// The local repository the backup ref is written in.
    pub repo_path: &'a Path,
    /// The decision the challenge showed, as [`plan_delete_head_branch`] handed it out; `None`
    /// when the user did not confirm, which sends nothing.
    pub answer: Option<&'a Decision>,
    /// What the user typed in the challenge's field, untouched.
    pub typed: &'a str,
    /// Called once, after the backup ref is written and read back, just before the request
    /// leaves, after the last point a cancel stops it.
    pub on_send: &'a (dyn Fn() + Sync),
}

impl std::fmt::Debug for BranchDeleteRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchDeleteRequest")
            .field("who", &self.who)
            .field("repo_path", &self.repo_path)
            .field("answer", &self.answer)
            .field("typed", &self.typed)
            .finish_non_exhaustive()
    }
}

/// Where a delete stands once its request was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchDeleteOutcome {
    /// GitHub deleted it.
    Deleted {
        /// How rc-git knows.
        settled: Settled,
    },
    /// GitHub answered and refused; nothing was deleted.
    Refused {
        /// GitHub's reason, single-line and scrubbed.
        reason: String,
    },
    /// The answer was lost, and a read afterwards found the branch still there at the same
    /// commit: it was not deleted when rc-git looked.
    NotDeleted {
        /// What went wrong with the answer.
        problem: String,
    },
    /// The answer was lost, and the read afterwards settles nothing.
    Unknown {
        /// What went wrong with the answer.
        problem: String,
        /// What the read found.
        after: BranchAfter,
    },
}

/// What the read after a lost answer found, when it settles nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAfter {
    /// A branch of that name, but not the one confirmed: another ref, or another commit.
    Changed {
        /// The commit it points at, when GitHub named one.
        now: Option<Oid>,
    },
    /// The read failed too.
    Unread {
        /// Why.
        reason: String,
    },
}

/// How a delete whose request left ended, with the backup ref it wrote first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDeleteReport {
    /// The branch's exact name.
    pub branch: String,
    /// The commit the branch was at.
    pub head_oid: Oid,
    /// The backup ref written before the request, which holds `head_oid` whatever the outcome.
    pub backup_ref: String,
    /// Whether the local repository has a branch of the same name, which the delete left alone.
    pub local_branch: bool,
    /// What GitHub did.
    pub outcome: BranchDeleteOutcome,
}

/// What the challenge must show for deleting the head branch of the pull request `who` names:
/// reads it now, refuses when it changed since the stamp or anything stands in the way, the head
/// commit missing from `repo_path` included, and answers the policy's decision. Sends nothing but
/// the reads, and writes nothing.
///
/// # Errors
/// [`ActionRefusal::Read`], [`ActionRefusal::Cancelled`], [`ActionRefusal::Changed`],
/// [`ActionRefusal::BranchBlocked`], [`ActionRefusal::Local`] for a repository that cannot be
/// read, and [`ActionRefusal::Denied`] for a protected branch.
pub fn plan_delete_head_branch(
    policy: &Policy,
    who: &ActionTarget<'_>,
    repo_path: &Path,
    cancel: &Cancel,
) -> Result<BranchDeletePlan, ActionRefusal> {
    let repo = local(repo_path, &who.pull_request())?;
    let delete = fresh(who, &repo, cancel)?;
    let (branch, head_oid, backup_ref) = (
        delete.branch().to_owned(),
        delete.head_oid(),
        delete.backup_ref(),
    );
    let decision = policy.evaluate(&Action::DeletePullRequestBranch(Box::new(delete)));
    if let Decision::Deny { reason } = decision {
        return Err(ActionRefusal::Denied { reason });
    }
    Ok(BranchDeletePlan {
        decision,
        branch,
        head_oid,
        backup_ref,
    })
}

/// Deletes the head branch `request` names, in the module's sequence: a fresh read compared with
/// the stamp, the blockers, the head commit in the local repository, the policy's own decision
/// checked against the answered one with the typed name, the backup ref written and read back,
/// then one mutation. A cancel before the request leaves sends nothing; after, it cannot stop it.
///
/// # Errors
/// An [`ActionRefusal`] whenever nothing was sent; once the request may have left, the answer is a
/// [`BranchDeleteReport`], never an error.
pub fn delete_head_branch(
    policy: &Policy,
    request: BranchDeleteRequest<'_>,
    cancel: &Cancel,
) -> Result<BranchDeleteReport, ActionRefusal> {
    let who = &request.who;
    let action = PullRequestAction::DeleteHeadBranch;
    let pull_request = who.pull_request();
    let repo = local(request.repo_path, &pull_request)?;
    let delete = fresh(who, &repo, cancel)?;
    let (branch, head_oid, ref_id) = (
        delete.branch().to_owned(),
        delete.head_oid(),
        delete.ref_id().clone(),
    );
    let built = Action::DeletePullRequestBranch(Box::new(delete));
    let answer = (request.answer, request.typed);
    let proof = confirmed(policy, &built, answer, (action, &pull_request))?;
    if cancel.is_cancelled() {
        return Err(ActionRefusal::Cancelled {
            action,
            pull_request,
        });
    }
    let backup_ref =
        back_up(&repo, &proof, &branch, head_oid).map_err(|reason| ActionRefusal::Local {
            action,
            pull_request: pull_request.clone(),
            reason,
        })?;
    (request.on_send)();
    let outcome = send(who, proof, (&ref_id, head_oid), cancel)?;
    Ok(BranchDeleteReport {
        local_branch: local_branch(&repo, &branch),
        branch,
        head_oid,
        backup_ref,
        outcome,
    })
}

/// The local repository at `repo_path`, opened as every guard action opens one.
fn local(repo_path: &Path, pull_request: &str) -> Result<gix::Repository, ActionRefusal> {
    open(repo_path, "delete the head branch", "repository").map_err(|error| ActionRefusal::Local {
        action: PullRequestAction::DeleteHeadBranch,
        pull_request: pull_request.to_owned(),
        reason: format!("the local repository could not be read ({error})"),
    })
}

/// The delete of the head branch `who` names, from the reads made just now, refused when it
/// changed since the stamp, when anything stands in the way, or when its commit is not in `repo`.
fn fresh(
    who: &ActionTarget<'_>,
    repo: &gix::Repository,
    cancel: &Cancel,
) -> Result<PullRequestBranchDelete, ActionRefusal> {
    let action = PullRequestAction::DeleteHeadBranch;
    let pull_request = who.pull_request();
    let session = Session::new(who.host, who.token);
    let read_failed = |source| match source {
        GitHubError::Cancelled { .. } => ActionRefusal::Cancelled {
            action,
            pull_request: pull_request.clone(),
        },
        source => ActionRefusal::Read {
            action,
            pull_request: pull_request.clone(),
            source,
        },
    };
    let mut read =
        read_head_branch(&session, who.target, who.stamp.number, cancel).map_err(&read_failed)?;
    if let Some(what) = changed(who.stamp, &read.stamp) {
        return Err(ActionRefusal::Changed {
            action,
            pull_request: pull_request.clone(),
            what,
        });
    }
    let shown = quoted(read.branch.shown());
    let blocked = |blocker| ActionRefusal::BranchBlocked {
        pull_request: pull_request.clone(),
        branch: shown.clone(),
        blocker,
    };
    if let Some(blocker) = branch_blocker(&read) {
        return Err(blocked(blocker));
    }
    let name = read.branch.exact().unwrap_or_default().to_owned();
    read.open_bases = read_open_bases(&session, who.target, &name, cancel).map_err(&read_failed)?;
    let delete = PullRequestBranchDelete::new(who.target, who.login, &read).map_err(blocked)?;
    if !has_commit(repo, delete.head_oid()) {
        return Err(blocked(ActionBlocker::NotFetched(delete.head_oid())));
    }
    Ok(delete)
}

/// Pull request `number` of `target` and its head branch, read with [`HEAD_BRANCH`].
fn read_head_branch(
    session: &Session<'_>,
    target: &RepositoryTarget,
    number: u64,
    cancel: &Cancel,
) -> GitHubResult<HeadBranch> {
    const OP: &str = "read the head branch";
    if number == 0 || i32::try_from(number).is_err() {
        return Err(GitHubError::InvalidValue {
            field: "pull request number",
            value: number.to_string(),
            reason: "a pull request number is 1 to 2147483647",
        });
    }
    session.check_target(target)?;
    let variables = head_branch_variables(target, number);
    let reply = session.graphql(OP, HEAD_BRANCH, &variables, cancel)?;
    head_branch(&reply, target)
}

/// The open pull requests based on `branch` of `target`, read with [`BASED_ON`]; `None` when the
/// answer does not count them.
fn read_open_bases(
    session: &Session<'_>,
    target: &RepositoryTarget,
    branch: &str,
    cancel: &Cancel,
) -> GitHubResult<Option<OpenPullRequests>> {
    const OP: &str = "read the pull requests based on the branch";
    session.check_target(target)?;
    let variables = based_on_variables(target, branch);
    let reply = session.graphql(OP, BASED_ON, &variables, cancel)?;
    Ok(open_of(repository(&reply)?.get("pullRequests")))
}

/// The head branch `reply` reports for `target`.
fn head_branch(reply: &GraphqlReply, target: &RepositoryTarget) -> GitHubResult<HeadBranch> {
    let repository = repository(reply)?;
    let node = repository
        .get("pullRequest")
        .filter(|node| !node.is_null())
        .ok_or_else(|| reply.failure("the repository has no such pull request"))?;
    let missing = || reply.malformed("the pull request lacks its number, branches or head commit");
    let number = node
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(missing)?;
    let head_oid = text(node, "headRefOid")
        .and_then(|hex| Oid::from_hex(hex).ok())
        .ok_or_else(missing)?;
    let name = text(node, "headRefName").ok_or_else(missing)?;
    let base = text(node, "baseRefName").ok_or_else(missing)?;
    let cross = flag(node, "isCrossRepository").unwrap_or(true);
    let head_ref = node.get("headRef").filter(|head_ref| !head_ref.is_null());
    Ok(HeadBranch {
        stamp: PullRequestStamp {
            number,
            head_oid,
            base_ref: single_line(base, MAX_LINE_CHARS),
            state: pull_request_state(text(node, "state")),
        },
        merged: flag(node, "merged") == Some(true),
        title: single_line(text(node, "title").unwrap_or_default(), MAX_LINE_CHARS),
        branch: BranchName::from_server(name),
        in_repository: !cross && names_target(node.get("headRepository"), target),
        viewer_can_delete: flag(node, "viewerCanDeleteHeadRef") == Some(true),
        head_ref: head_ref.map(|head_ref| head_ref_of(head_ref, name, target)),
        default_branch: repository
            .get("defaultBranchRef")
            .and_then(|default| text(default, "name"))
            .map(|default| default.eq_ignore_ascii_case(name)),
        open_bases: None,
    })
}

/// The `headRef` node `value` of a pull request whose head branch is `name` in `target`.
fn head_ref_of(value: &Value, name: &str, target: &RepositoryTarget) -> HeadRef {
    let is_head_branch = text(value, "prefix") == Some("refs/heads/")
        && text(value, "name") == Some(name)
        && names_target(value.get("repository"), target);
    HeadRef {
        id: text(value, "id").and_then(RefId::parse),
        target: value
            .get("target")
            .and_then(|object| text(object, "oid"))
            .and_then(|hex| Oid::from_hex(hex).ok()),
        is_head_branch,
        open_heads: open_of(value.get("associatedPullRequests")),
    }
}

/// Whether the repository node `value` (`name` and `owner { login }`) is `target`, compared
/// without ASCII case as GitHub compares them.
fn names_target(value: Option<&Value>, target: &RepositoryTarget) -> bool {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return false;
    };
    let owner = value.get("owner").and_then(|owner| text(owner, "login"));
    text(value, "name").is_some_and(|name| name.eq_ignore_ascii_case(target.name()))
        && owner.is_some_and(|owner| owner.eq_ignore_ascii_case(target.owner()))
}

/// The pull request connection `value` counted: `None` when it has no `totalCount`.
fn open_of(value: Option<&Value>) -> Option<OpenPullRequests> {
    let connection = value.filter(|value| !value.is_null())?;
    let total = connection.get("totalCount").and_then(Value::as_u64)?;
    let first = nodes(connection).find_map(|node| node.get("number").and_then(Value::as_u64));
    Some(OpenPullRequests { total, first })
}

/// Whether `oid` is a commit in `repo`'s object database: only then can a backup ref hold it.
fn has_commit(repo: &gix::Repository, oid: Oid) -> bool {
    let Ok(id) = gix::ObjectId::try_from(oid.as_bytes()) else {
        return false;
    };
    repo.find_header(id)
        .is_ok_and(|header| header.kind() == gix::object::Kind::Commit)
}

/// Writes the backup ref of `branch` at `head` through the guard's [`Writer`], which only the
/// confirmation builds, and reads it back: the name it wrote, or why nothing may be sent.
fn back_up(
    repo: &gix::Repository,
    proof: &Confirmation,
    branch: &str,
    head: Oid,
) -> Result<String, String> {
    let writer = Writer::new(repo, proof);
    if !writer.backup_required() {
        return Err(
            "the confirmation asks for no backup ref, which deleting a branch always has"
                .to_owned(),
        );
    }
    let id = gix::ObjectId::try_from(head.as_bytes())
        .map_err(|error| format!("commit {head} is not an object id rc-git can write: {error}"))?;
    let name = writer
        .write_backup_ref(BACKUP_KIND, branch, id)
        .map_err(|error| format!("the backup ref could not be written ({error})"))?;
    let written = repo
        .find_reference(name.as_str())
        .map_err(|error| format!("the backup ref {name} could not be read back ({error})"))?;
    if written.target().try_id() != Some(id.as_ref()) {
        return Err(format!(
            "the backup ref {name} does not point at {head} after the write"
        ));
    }
    Ok(name)
}

/// Whether `repo` has a local branch named `branch`.
fn local_branch(repo: &gix::Repository, branch: &str) -> bool {
    repo.try_find_reference(format!("refs/heads/{branch}").as_str())
        .is_ok_and(|found| found.is_some())
}

/// Sends the one mutation `proof` confirms and reads what came of it: an answer, or for a lost
/// one, the one read that settles what it can.
fn send(
    who: &ActionTarget<'_>,
    proof: Confirmation,
    confirmed: (&RefId, Oid),
    cancel: &Cancel,
) -> Result<BranchDeleteOutcome, ActionRefusal> {
    let action = PullRequestAction::DeleteHeadBranch;
    let pull_request = who.pull_request();
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
                value: "the confirmed delete".to_owned(),
                reason: "it names no request to send",
            }));
        }
        Some(Mutated::NotSent(source)) => return Err(not_sent(source)),
        Some(Mutated::Answered(answer)) => match answered(first_look(answer)) {
            Ok(outcome) => return Ok(outcome),
            Err(problem) => problem,
        },
        Some(Mutated::Lost(error)) => error.to_string(),
    };
    Ok(settle(who, confirmed, problem, cancel))
}

/// What the host's answer says: a final outcome, or the problem to report beside the read that
/// follows. A non-null `deleteRef` payload is the success GitHub reports; only an empty payload
/// with errors that each show the mutation did not run or was refused is a refusal.
fn answered(look: FirstLook) -> Result<BranchDeleteOutcome, String> {
    let reply = match look {
        FirstLook::Reply(reply) => reply,
        FirstLook::Refused(reason) => return Ok(BranchDeleteOutcome::Refused { reason }),
        FirstLook::Unknown(problem) => return Err(problem),
    };
    let payload = reply
        .data
        .as_ref()
        .and_then(|data| data.get("deleteRef"))
        .filter(|payload| !payload.is_null());
    if payload.is_some_and(Value::is_object) {
        return Ok(BranchDeleteOutcome::Deleted {
            settled: Settled::Answer,
        });
    }
    let words = ("the branch was deleted", "that the branch was deleted");
    errors_verdict(&reply, payload.is_some(), words)
        .map(|reason| BranchDeleteOutcome::Refused { reason })
}

/// The outcome of a request whose answer was lost: one read of the head branch, never a second
/// mutation. Gone is deleted; the confirmed ref at the confirmed commit is not deleted; anything
/// else is unknown.
fn settle(
    who: &ActionTarget<'_>,
    (ref_id, head): (&RefId, Oid),
    problem: String,
    cancel: &Cancel,
) -> BranchDeleteOutcome {
    let session = Session::new(who.host, who.token);
    let read = match read_head_branch(&session, who.target, who.stamp.number, &settling(cancel)) {
        Ok(read) => read,
        Err(error) => {
            return BranchDeleteOutcome::Unknown {
                problem,
                after: BranchAfter::Unread {
                    reason: error.to_string(),
                },
            };
        }
    };
    match read.head_ref {
        None => BranchDeleteOutcome::Deleted {
            settled: Settled::Reread,
        },
        Some(now) if now.id.as_ref() == Some(ref_id) && now.target == Some(head) => {
            BranchDeleteOutcome::NotDeleted { problem }
        }
        Some(now) => BranchDeleteOutcome::Unknown {
            problem,
            after: BranchAfter::Changed { now: now.target },
        },
    }
}
