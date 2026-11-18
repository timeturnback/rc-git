//! The GitHub mutations rc-git sends: a closed set of static documents, each built only from
//! the [`Confirmation`] of the action it performs. [`Mutation::of`] reads the confirmed action
//! and nothing else, so the pull request, the expected head and the method a request carries
//! are exactly the ones the user confirmed, and a proof for any other action yields no
//! mutation at all. Every value travels in `variables`.

use rc_git_core::Oid;
use serde_json::{Value, json};

use super::RepositoryTarget;
use super::merge::{PullRequestAdminMerge, PullRequestMerge};
use super::pull_types::{MergeMethod, PullRequestId};
use crate::{Action, Confirmation};

/// Merges a pull request into its base, refused by GitHub unless its head is still
/// `$head`. Checked against GitHub's GraphQL reference: `mergePullRequest` takes
/// `MergePullRequestInput` (`pullRequestId: ID!`, `expectedHeadOid: GitObjectID`, `mergeMethod:
/// PullRequestMergeMethod`) and returns the merged `pullRequest`. The admin merge sends this same
/// document: the input has no field for going past branch protection, GitHub decides that from
/// the account, and the GitHub CLI's `gh pr merge --admin` sends this mutation too.
pub(super) const MERGE: &str = "mutation RcGitMergePullRequest($id: ID!, $head: GitObjectID!, \
$method: PullRequestMergeMethod!) { mergePullRequest(input: {pullRequestId: $id, \
expectedHeadOid: $head, mergeMethod: $method}) { pullRequest { number state merged headRefOid \
mergeCommit { oid } } } }";

/// One mutation a proof confirms.
#[derive(Debug, Clone, Copy)]
pub(super) enum Mutation<'a> {
    /// [`MERGE`] of a pull request.
    Merge(&'a PullRequestMerge),
    /// [`MERGE`] of a pull request branch protection blocks, by an account GitHub says may
    /// merge it past that protection.
    AdminMerge(&'a PullRequestAdminMerge),
}

impl<'a> Mutation<'a> {
    /// The mutation `proof` confirms; `None` for an action rc-git performs on no GitHub host.
    /// The match names every action, so a new one does not compile until it is placed here.
    pub(super) fn of(proof: &'a Confirmation) -> Option<Self> {
        match proof.action() {
            Action::MergePullRequest(merge) => Some(Self::Merge(merge)),
            Action::AdminMergePullRequest(merge) => Some(Self::AdminMerge(merge)),
            Action::Fetch
            | Action::FetchPullRequest { .. }
            | Action::CreateLocalBranch { .. }
            | Action::CreateTag { .. }
            | Action::Checkout { .. }
            | Action::PullFastForward { .. }
            | Action::Push { .. }
            | Action::DeleteLocalBranch { .. }
            | Action::DeleteRemoteBranch { .. }
            | Action::DeleteTag { .. }
            | Action::ResetHard { .. }
            | Action::RewriteHistory { .. }
            | Action::DiscardWorkingChanges { .. }
            | Action::Prune
            | Action::Commit { .. }
            | Action::StashPush { .. }
            | Action::StashApply
            | Action::StashPop { .. }
            | Action::StashDrop { .. } => None,
        }
    }

    /// What is being done, for messages.
    pub(super) fn op(self) -> &'static str {
        match self {
            Self::Merge(_) => "merge the pull request",
            Self::AdminMerge(_) => "merge the pull request past branch protection",
        }
    }

    /// The repository it acts on, which must be on the session's host.
    pub(super) fn target(self) -> &'a RepositoryTarget {
        match self {
            Self::Merge(merge) => merge.target(),
            Self::AdminMerge(merge) => merge.target(),
        }
    }

    /// The static document.
    pub(super) fn document(self) -> &'static str {
        match self {
            Self::Merge(_) | Self::AdminMerge(_) => MERGE,
        }
    }

    /// The variables, from the confirmed action alone.
    pub(super) fn variables(self) -> Value {
        match self {
            Self::Merge(merge) => variables_of(merge.id(), merge.head_oid(), merge.method()),
            Self::AdminMerge(merge) => variables_of(merge.id(), merge.head_oid(), merge.method()),
        }
    }
}

/// The variables of [`MERGE`]: the pull request `id`, the `head` it must still be at, and the
/// `method`.
pub(super) fn variables_of(id: &PullRequestId, head: Oid, method: MergeMethod) -> Value {
    json!({
        "id": id.as_str(),
        "head": head.to_hex(),
        "method": method_word(method),
    })
}

/// `method` as GraphQL's `PullRequestMergeMethod`.
fn method_word(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "MERGE",
        MergeMethod::Squash => "SQUASH",
        MergeMethod::Rebase => "REBASE",
    }
}
