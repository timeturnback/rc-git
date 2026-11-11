//! Port of the action half of `RepositoryGuard.swift`: every mutating guard action the models
//! call, as one trait a test fake can stand in for, and the live implementation over
//! `rc_git_guard::actions`. The value logic around them (decisions, warnings, failure
//! classes, the cancel token) lives in [`crate::guard_bridge`].
//!
//! This module holds rc-git-kit's one `Policy::default()` call: [`policy`]. Every action and
//! every plan reads the policy through it, so the app never builds a second one.
//!
//! The trait is bound to one repository path. Sync and Back to default are workflows over
//! these steps (`upstream_status`, `fetch`, `pull_fast_forward`, `push`, `default_branch`,
//! `checkout`), composed by the sync model; there is no guard action of their own.
//!
//! Every method blocks, and the remote ones build their own tokio runtime: call them through
//! [`crate::blocking::blocking`], never inside a runtime and never on the UI thread.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rc_git_core::{DefaultBranch, Oid, StashEntry};
use rc_git_guard::Policy;
use rc_git_guard::actions::{
    self, ActionError, ActionResult, CheckoutOutcome, CommitRequest, Committed, Credentials,
    Deleted, DiscardOutcome, FetchOutcome, HunkStamp, Identity, PullOutcome, PushOutcome,
    RemoteAuth, StashRequest, StashSaved, UpstreamStatus,
};

use crate::guard_bridge::{CancelToken, GuardDecision};

/// The protection policy every action and plan runs under: the protected patterns
/// (`main`, `master`, `release/*`) are fixed and no setting changes them.
pub fn policy() -> &'static Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    POLICY.get_or_init(Policy::default)
}

/// What committing `files` selected files would need. Pure policy.
#[must_use]
pub fn plan_commit(files: usize) -> GuardDecision {
    GuardDecision::new(&actions::plan_commit(policy(), files), false)
}

/// What stashing `files` selected files would need. Pure policy.
#[must_use]
pub fn plan_stash_push(files: usize) -> GuardDecision {
    GuardDecision::new(&actions::plan_stash_push(policy(), files), false)
}

/// What applying a stash entry would need. Pure policy.
#[must_use]
pub fn plan_stash_apply() -> GuardDecision {
    GuardDecision::new(&actions::plan_stash_apply(policy()), false)
}

/// What popping `entry` would need; the challenge names its position and message.
#[must_use]
pub fn plan_stash_pop(entry: &StashEntry) -> GuardDecision {
    let decision = actions::plan_stash_pop(policy(), entry.index, &entry.message);
    GuardDecision::new(&decision, false)
}

/// What dropping `entry` would need; the challenge names its position and message.
#[must_use]
pub fn plan_stash_drop(entry: &StashEntry) -> GuardDecision {
    let decision = actions::plan_stash_drop(policy(), entry.index, &entry.message);
    GuardDecision::new(&decision, false)
}

/// What discarding changes in `files` files would need. Pure policy.
#[must_use]
pub fn plan_discard(files: usize) -> GuardDecision {
    GuardDecision::new(&actions::plan_discard(policy(), files), false)
}

/// What deleting tag `name` would need. Pure policy.
#[must_use]
pub fn plan_tag_delete(name: &str) -> GuardDecision {
    GuardDecision::new(&actions::plan_tag_delete(policy(), name), false)
}

/// How a remote step authenticates: the credentials to offer, the host they were issued for
/// (no other host gets them; `None` means the host the remote URL names), and a host-key
/// fingerprint the user approved for an otherwise unknown ssh host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRequest {
    /// An HTTPS username and token, or (token only) an ssh key passphrase.
    pub credentials: Option<Credentials>,
    /// The host the credentials belong to.
    pub host: Option<String>,
    /// A `SHA256:...` fingerprint the user chose to trust.
    pub accepted_host_key: Option<String>,
}

/// The mutating actions the models call on one repository, plus the two cheap reads the sync
/// workflow decides with. [`LiveActions`] is the real implementation; tests inject a fake.
pub trait RepositoryActions: Send + Sync {
    /// What deleting branch `name` would need, with whether its commits are merged.
    ///
    /// # Errors
    /// When the branch is not listed or the repository cannot be read.
    fn plan_branch_delete(&self, name: &str) -> ActionResult<GuardDecision>;

    /// Creates branch `name` at `revspec`, never over an existing one.
    ///
    /// # Errors
    /// When the policy refuses, the name is invalid or taken, or `revspec` names nothing.
    fn create_branch(&self, name: &str, revspec: &str) -> ActionResult<()>;

    /// Creates lightweight tag `name` at `revspec`, never over an existing one.
    ///
    /// # Errors
    /// As [`Self::create_branch`].
    fn create_tag(&self, name: &str, revspec: &str) -> ActionResult<()>;

    /// Deletes branch `name`; `input` answers the challenge.
    ///
    /// # Errors
    /// When the policy refuses, the challenge fails, or the branch is checked out.
    fn delete_branch(&self, name: &str, input: Option<&str>) -> ActionResult<Deleted>;

    /// Deletes tag `name`, refused when it no longer points at `expected`.
    ///
    /// # Errors
    /// When the policy refuses, the challenge fails, or the tag moved.
    fn delete_tag(&self, name: &str, expected: Oid, input: Option<&str>) -> ActionResult<Deleted>;

    /// The repository's own `user.name`/`user.email`, read fresh; `None` when it sets
    /// either one empty or not at all.
    ///
    /// # Errors
    /// When the repository cannot be opened.
    fn configured_identity(&self) -> ActionResult<Option<Identity>>;

    /// Commits the selections to the checked-out branch.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::commit` describes.
    fn commit(&self, request: CommitRequest<'_>, input: Option<&str>) -> ActionResult<Committed>;

    /// Stashes the changes at the request's paths and restores them to `HEAD`.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::stash_push` describes.
    fn stash_push(
        &self,
        request: StashRequest<'_>,
        input: Option<&str>,
    ) -> ActionResult<StashSaved>;

    /// Writes `entry` back to the working tree, keeping it; refused when the stash at its
    /// index is no longer its commit.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::stash_apply` describes.
    fn stash_apply(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<()>;

    /// Applies `entry` and drops it, refused as [`Self::stash_apply`] is.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::stash_pop` describes.
    fn stash_pop(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<Deleted>;

    /// Deletes `entry` without applying it, refused as [`Self::stash_apply`] is.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::stash_drop` describes.
    fn stash_drop(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<Deleted>;

    /// Discards every change at `paths`.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::discard_files` describes.
    fn discard_files(
        &self,
        paths: &[String],
        input: Option<&str>,
    ) -> ActionResult<Vec<DiscardOutcome>>;

    /// Discards only `stamps` of `path`, keeping its other changes.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::discard_hunks` describes.
    fn discard_hunks(
        &self,
        path: &str,
        stamps: &[HunkStamp],
        input: Option<&str>,
    ) -> ActionResult<()>;

    /// The checked-out branch's upstream and ahead/behind counts. Local, no network.
    ///
    /// # Errors
    /// When `HEAD` is detached or the repository cannot be read.
    fn upstream_status(&self) -> ActionResult<UpstreamStatus>;

    /// The repository's mainline branch, or `None` when nothing names one. Local and cheap.
    ///
    /// # Errors
    /// When the repository or its refs cannot be read.
    fn default_branch(&self) -> ActionResult<Option<DefaultBranch>>;

    /// Fetches from the branch's upstream remote; `cancel` stops it between steps.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::fetch_upstream` describes.
    fn fetch(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<FetchOutcome>;

    /// Fast-forwards the checked-out branch to its tracking ref. No network; fetch first.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::pull_fast_forward` describes.
    fn pull_fast_forward(&self) -> ActionResult<PullOutcome>;

    /// Checks out local branch `branch`, refusing the whole switch over uncommitted work.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::checkout_branch` describes.
    fn checkout(&self, branch: &str) -> ActionResult<CheckoutOutcome>;

    /// Pushes the checked-out branch to its upstream; `cancel` stops it between steps.
    ///
    /// # Errors
    /// As `rc_git_guard::actions::push_upstream` describes.
    fn push(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<PushOutcome>;
}

/// The guard actions on the repository at one path.
#[derive(Debug, Clone)]
pub struct LiveActions {
    repo_path: PathBuf,
    ssh_dir: Option<PathBuf>,
}

impl LiveActions {
    /// Actions on the repository at `repo_path`, reading ssh setup from `~/.ssh`.
    #[must_use]
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
            ssh_dir: None,
        }
    }

    /// The same actions reading ssh `config`, `known_hosts` and keys from `ssh_dir`; tests
    /// point it at a fixture directory, never the developer's own.
    #[must_use]
    pub fn with_ssh_dir(self, ssh_dir: impl Into<PathBuf>) -> Self {
        Self {
            ssh_dir: Some(ssh_dir.into()),
            ..self
        }
    }

    /// The repository the actions run on.
    #[must_use]
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    fn auth<'a>(&'a self, remote: &'a RemoteRequest) -> RemoteAuth<'a> {
        RemoteAuth {
            creds: remote.credentials.as_ref(),
            host: remote.host.as_deref(),
            accepted_hostkey: remote.accepted_host_key.as_deref(),
            ssh_dir: self.ssh_dir.as_deref(),
        }
    }
}

impl RepositoryActions for LiveActions {
    fn plan_branch_delete(&self, name: &str) -> ActionResult<GuardDecision> {
        let plan = actions::plan_branch_delete(&self.repo_path, policy(), name)?;
        Ok(GuardDecision::new(&plan.decision, plan.merged))
    }

    fn create_branch(&self, name: &str, revspec: &str) -> ActionResult<()> {
        actions::create_branch(&self.repo_path, policy(), name, revspec)
    }

    fn create_tag(&self, name: &str, revspec: &str) -> ActionResult<()> {
        actions::create_tag(&self.repo_path, policy(), name, revspec)
    }

    fn delete_branch(&self, name: &str, input: Option<&str>) -> ActionResult<Deleted> {
        actions::delete_branch(&self.repo_path, policy(), name, input)
    }

    fn delete_tag(&self, name: &str, expected: Oid, input: Option<&str>) -> ActionResult<Deleted> {
        actions::delete_tag(&self.repo_path, policy(), name, expected, input)
    }

    fn configured_identity(&self) -> ActionResult<Option<Identity>> {
        actions::configured_identity(&self.repo_path)
    }

    fn commit(&self, request: CommitRequest<'_>, input: Option<&str>) -> ActionResult<Committed> {
        actions::commit(&self.repo_path, policy(), request, input)
    }

    fn stash_push(
        &self,
        request: StashRequest<'_>,
        input: Option<&str>,
    ) -> ActionResult<StashSaved> {
        actions::stash_push(&self.repo_path, policy(), request, input)
    }

    fn stash_apply(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<()> {
        actions::stash_apply(&self.repo_path, policy(), entry.index, entry.oid, input)
    }

    fn stash_pop(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<Deleted> {
        actions::stash_pop(&self.repo_path, policy(), entry.index, entry.oid, input)
    }

    fn stash_drop(&self, entry: &StashEntry, input: Option<&str>) -> ActionResult<Deleted> {
        actions::stash_drop(&self.repo_path, policy(), entry.index, entry.oid, input)
    }

    fn discard_files(
        &self,
        paths: &[String],
        input: Option<&str>,
    ) -> ActionResult<Vec<DiscardOutcome>> {
        actions::discard_files(&self.repo_path, policy(), paths, input)
    }

    fn discard_hunks(
        &self,
        path: &str,
        stamps: &[HunkStamp],
        input: Option<&str>,
    ) -> ActionResult<()> {
        actions::discard_hunks(&self.repo_path, policy(), path, stamps, input)
    }

    fn upstream_status(&self) -> ActionResult<UpstreamStatus> {
        actions::upstream_status(&self.repo_path)
    }

    fn default_branch(&self) -> ActionResult<Option<DefaultBranch>> {
        const OP: &str = "read default branch";
        let name = self.repo_path.display().to_string();
        let git = |source: rc_git_core::Error| ActionError::Git {
            op: OP,
            name: name.clone(),
            source: Box::new(source),
        };
        let repository = rc_git_core::Repository::open(&self.repo_path).map_err(git)?;
        repository.default_branch().map_err(git)
    }

    fn fetch(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<FetchOutcome> {
        actions::fetch_upstream(
            &self.repo_path,
            policy(),
            self.auth(remote),
            cancel.remote(),
        )
    }

    fn pull_fast_forward(&self) -> ActionResult<PullOutcome> {
        actions::pull_fast_forward(&self.repo_path, policy())
    }

    fn checkout(&self, branch: &str) -> ActionResult<CheckoutOutcome> {
        actions::checkout_branch(&self.repo_path, policy(), branch)
    }

    fn push(&self, remote: &RemoteRequest, cancel: &CancelToken) -> ActionResult<PushOutcome> {
        actions::push_upstream(
            &self.repo_path,
            policy(),
            self.auth(remote),
            cancel.remote(),
        )
    }
}
