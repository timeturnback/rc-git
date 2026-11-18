//! The GitHub mutations rc-git sends: a closed set of static GraphQL documents and REST paths,
//! each built only from the [`Confirmation`] of the action it performs. [`Mutation::of`] reads
//! the confirmed action and nothing else, so the pull request, the expected head, the method and
//! the workflow runs a request carries are exactly the ones the user confirmed, and a proof for
//! any other action yields no mutation, or another action's own. Every GraphQL value travels in
//! `variables`; a REST path is plain segments and integer ids only.

use rc_git_core::Oid;
use serde_json::{Value, json};

use super::RepositoryTarget;
use super::auto_merge::{AutoMergeDisable, AutoMergeEnable};
use super::merge::{PullRequestAdminMerge, PullRequestMerge};
use super::pull_types::{MergeMethod, PullRequestId};
use super::rerun::{PullRequestRerun, rerun_body};
use crate::{Action, Confirmation};

/// Merges a pull request into its base, refused by GitHub unless its head is still
/// `$head`. Checked against GitHub's GraphQL reference: `mergePullRequest` takes
/// `MergePullRequestInput` (`pullRequestId: ID!`, `expectedHeadOid: GitObjectID`, `mergeMethod:
/// PullRequestMergeMethod`) and returns the merged `pullRequest`. The admin merge sends this same
/// document: the input has no field for going past branch protection, GitHub decides that from
/// the account, and the GitHub CLI's `gh pr merge --admin` sends this mutation too.
///
/// The input has no field for the base either, so a base retargeted after rc-git's last read
/// cannot be refused, only seen: the answer selects `baseRefName` (`String!`, "the name of the
/// base Ref associated with the pull request") and the merge compares it with the base the user
/// confirmed.
pub(super) const MERGE: &str = "mutation RcGitMergePullRequest($id: ID!, $head: GitObjectID!, \
$method: PullRequestMergeMethod!) { mergePullRequest(input: {pullRequestId: $id, \
expectedHeadOid: $head, mergeMethod: $method}) { pullRequest { number state merged headRefOid \
baseRefName mergeCommit { oid } } } }";

/// Turns on auto-merge, refused by GitHub unless the head is still `$head`. Checked against
/// GitHub's GraphQL reference: `enablePullRequestAutoMerge` takes
/// `EnablePullRequestAutoMergeInput` (`pullRequestId: ID!`, `expectedHeadOid: GitObjectID`,
/// `mergeMethod: PullRequestMergeMethod`, the merge method "if omitted, defaults to MERGE", so
/// rc-git always sends it) and returns the `pullRequest`, whose `autoMergeRequest` (an
/// `AutoMergeRequest`, `mergeMethod: PullRequestMergeMethod!`) shows it on. rc-git sends none of
/// the commit message fields, nor `includeStack`.
pub(super) const ENABLE_AUTO_MERGE: &str = "mutation RcGitEnableAutoMerge($id: ID!, \
$head: GitObjectID!, $method: PullRequestMergeMethod!) { enablePullRequestAutoMerge(input: \
{pullRequestId: $id, expectedHeadOid: $head, mergeMethod: $method}) { pullRequest { number state \
headRefOid baseRefName autoMergeRequest { mergeMethod } } } }";

/// Turns off auto-merge. Checked against GitHub's GraphQL reference:
/// `disablePullRequestAutoMerge` takes `DisablePullRequestAutoMergeInput` (`pullRequestId: ID!`)
/// and returns the `pullRequest`, whose `autoMergeRequest` is null once it is off. rc-git does not
/// send `includeStack`.
pub(super) const DISABLE_AUTO_MERGE: &str = "mutation RcGitDisableAutoMerge($id: ID!) { \
disablePullRequestAutoMerge(input: {pullRequestId: $id}) { pullRequest { number state \
headRefOid autoMergeRequest { mergeMethod } } } }";

/// One request of a confirmed mutation, built from the action alone.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MutationRequest {
    /// A static GraphQL document and its variables.
    Graphql {
        /// The document.
        document: &'static str,
        /// The variables.
        variables: Value,
    },
    /// A REST `POST` to the API path `segments` (plain names, checked again when the URL is
    /// built) with a JSON `body`.
    Rest {
        /// The path's segments.
        segments: Vec<String>,
        /// The body.
        body: Value,
    },
}

/// One mutation a proof confirms.
#[derive(Debug, Clone, Copy)]
pub(super) enum Mutation<'a> {
    /// [`MERGE`] of a pull request.
    Merge(&'a PullRequestMerge),
    /// [`MERGE`] of a pull request branch protection blocks, by an account GitHub says may
    /// merge it past that protection.
    AdminMerge(&'a PullRequestAdminMerge),
    /// [`ENABLE_AUTO_MERGE`].
    EnableAutoMerge(&'a AutoMergeEnable),
    /// [`DISABLE_AUTO_MERGE`].
    DisableAutoMerge(&'a AutoMergeDisable),
    /// One REST re-run of failed jobs per confirmed workflow run, in the confirmed order.
    RerunFailedJobs(&'a PullRequestRerun),
}

impl<'a> Mutation<'a> {
    /// The mutation `proof` confirms; `None` for an action rc-git performs on no GitHub host.
    /// The match names every action, so a new one does not compile until it is placed here.
    pub(super) fn of(proof: &'a Confirmation) -> Option<Self> {
        match proof.action() {
            Action::MergePullRequest(merge) => Some(Self::Merge(merge)),
            Action::AdminMergePullRequest(merge) => Some(Self::AdminMerge(merge)),
            Action::EnableAutoMerge(enable) => Some(Self::EnableAutoMerge(enable)),
            Action::DisableAutoMerge(disable) => Some(Self::DisableAutoMerge(disable)),
            Action::RerunFailedJobs(rerun) => Some(Self::RerunFailedJobs(rerun)),
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
            Self::EnableAutoMerge(_) => "turn on auto-merge",
            Self::DisableAutoMerge(_) => "turn off auto-merge",
            Self::RerunFailedJobs(_) => "re-run failed jobs",
        }
    }

    /// The repository it acts on, which must be on the session's host.
    pub(super) fn target(self) -> &'a RepositoryTarget {
        match self {
            Self::Merge(merge) => merge.target(),
            Self::AdminMerge(merge) => merge.target(),
            Self::EnableAutoMerge(enable) => enable.target(),
            Self::DisableAutoMerge(disable) => disable.target(),
            Self::RerunFailedJobs(rerun) => rerun.target(),
        }
    }

    /// Request `index` (from 0), from the confirmed action alone; `None` past the last.
    pub(super) fn request(self, index: usize) -> Option<MutationRequest> {
        let graphql = |document, variables| MutationRequest::Graphql {
            document,
            variables,
        };
        match self {
            Self::RerunFailedJobs(rerun) => {
                let run = rerun.runs().get(index)?;
                let target = rerun.target();
                let segments = [
                    "repos",
                    target.owner(),
                    target.name(),
                    "actions",
                    "runs",
                    &run.id.to_string(),
                    "rerun-failed-jobs",
                ];
                Some(MutationRequest::Rest {
                    segments: segments
                        .iter()
                        .map(|segment| (*segment).to_owned())
                        .collect(),
                    body: rerun_body(),
                })
            }
            Self::Merge(_)
            | Self::AdminMerge(_)
            | Self::EnableAutoMerge(_)
            | Self::DisableAutoMerge(_)
                if index > 0 =>
            {
                None
            }
            Self::Merge(merge) => Some(graphql(
                MERGE,
                variables_of(merge.id(), merge.head_oid(), merge.method()),
            )),
            Self::AdminMerge(merge) => Some(graphql(
                MERGE,
                variables_of(merge.id(), merge.head_oid(), merge.method()),
            )),
            Self::EnableAutoMerge(enable) => Some(graphql(
                ENABLE_AUTO_MERGE,
                variables_of(enable.id(), enable.head_oid(), enable.method()),
            )),
            Self::DisableAutoMerge(disable) => Some(graphql(
                DISABLE_AUTO_MERGE,
                json!({ "id": disable.id().as_str() }),
            )),
        }
    }
}

/// The variables of [`MERGE`] and [`ENABLE_AUTO_MERGE`]: the pull request `id`, the `head` it
/// must still be at, and the `method`.
pub(super) fn variables_of(id: &PullRequestId, head: Oid, method: MergeMethod) -> Value {
    json!({
        "id": id.as_str(),
        "head": head.to_hex(),
        "method": method_word(method),
    })
}

/// `method` as GraphQL's `PullRequestMergeMethod`.
pub(super) fn method_word(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "MERGE",
        MergeMethod::Squash => "SQUASH",
        MergeMethod::Rebase => "REBASE",
    }
}
