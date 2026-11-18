//! The one write path in rc-git. Every entry point here re-evaluates the policy for its
//! [`Action`] and turns the caller's input into a [`Confirmation`] (through
//! [`Challenge::verify`](crate::Challenge::verify) when the decision is a challenge) before
//! anything is written; every write is a method on a `Writer`, which only that proof can
//! build. Destructive deletions write a backup ref under `refs/rc-git/backup/` before anything
//! is removed.
//!
//! Repositories open isolated, exactly as the read-only core does: only the repository's own
//! configuration is consulted.

use std::path::Path;

use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use crate::{Action, Confirmation, Decision, Policy, Refusal};

mod checkout;
mod commit;
mod discard;
mod remote;
mod stash;
mod treeapply;
mod treebuild;
mod workfile;
mod write;

use write::Writer;

pub use checkout::{CheckoutOutcome, checkout_branch};
pub use commit::{CommitRequest, Committed, commit, configured_identity};
pub use discard::{DiscardKind, DiscardOutcome, discard_files, discard_hunks};
pub use remote::github;
pub use remote::{
    Cancel, Credentials, FetchOutcome, FetchRemote, PULL_HEAD_NAMESPACE, PullHeadFetch,
    PullHeadRequest, PullOutcome, PushOutcome, RefUpdate, RemoteAuth, UpstreamStatus,
    fetch_pull_request_head, fetch_remote, fetch_upstream, pull_fast_forward, pull_head_ref,
    push_upstream, upstream_status,
};
pub use stash::{StashRequest, StashSaved, stash_apply, stash_drop, stash_pop, stash_push};

/// Who a commit or stash is recorded as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `user.name`.
    pub name: String,
    /// `user.email`.
    pub email: String,
}

/// Where a commit or stash takes its identity from, in order: `explicit` as given, else the
/// repository's own `user.name`/`user.email`, else `fallback`, else
/// [`ActionError::MissingIdentity`]. The app passes the identity it remembered for the
/// repository as `fallback`, so a repository that sets its own identity always keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Author {
    /// Recorded ahead of the repository's config, as `git commit --author` is.
    pub explicit: Option<Identity>,
    /// Recorded only when the repository's own config sets no identity.
    pub fallback: Option<Identity>,
}

/// One hunk of a worktree diff, identified by its unified header ranges. The write path
/// recomputes the diff and requires every stamp to match a current hunk exactly, so a file
/// edited after selection is refused instead of spliced wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HunkStamp {
    /// 1-based first line of the hunk on the tree side.
    pub old_start: u32,
    /// Lines the hunk covers on the tree side, context included.
    pub old_lines: u32,
    /// 1-based first line of the hunk on the disk side.
    pub new_start: u32,
    /// Lines the hunk covers on the disk side, context included.
    pub new_lines: u32,
}

impl From<&rc_git_core::DiffHunk> for HunkStamp {
    fn from(hunk: &rc_git_core::DiffHunk) -> Self {
        Self {
            old_start: hunk.old_start,
            old_lines: hunk.old_lines,
            new_start: hunk.new_start,
            new_lines: hunk.new_lines,
        }
    }
}

/// What of one file goes into a commit or a stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSelection {
    /// The whole file as it is on disk; addition, modification, or deletion is inferred.
    Whole {
        /// Repository-relative path with `/` separators.
        path: String,
    },
    /// Only the given hunks of a tracked, modified text file.
    Hunks {
        /// Repository-relative path with `/` separators.
        path: String,
        /// The selected hunks, in file order.
        hunks: Vec<HunkStamp>,
    },
}

/// Reflog committer name for every ref edit rc-git makes outside the stash.
pub(super) const REFLOG_NAME: &str = "rc-git";
/// Reflog committer email paired with [`REFLOG_NAME`].
pub(super) const REFLOG_EMAIL: &str = "rc-git@localhost";

/// A direct (never dereferenced) update of `full_name` to `new` that logs `message` to the
/// ref's reflog, provided one exists.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and `full_name` when the name is not a valid ref name.
fn update_edit(
    full_name: &str,
    expected: PreviousValue,
    new: Target,
    message: &str,
    op: &'static str,
) -> ActionResult<RefEdit> {
    Ok(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: message.into(),
            },
            expected,
            new,
        },
        name: full_name.try_into().map_err(git_err(op, full_name))?,
        deref: false,
    })
}

/// A direct (never dereferenced) deletion of `full_name` together with its reflog.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and `full_name` when the name is not a valid ref name.
fn delete_edit(
    full_name: &str,
    expected: PreviousValue,
    op: &'static str,
) -> ActionResult<RefEdit> {
    Ok(RefEdit {
        change: Change::Delete {
            expected,
            log: RefLog::AndReference,
        },
        name: full_name.try_into().map_err(git_err(op, full_name))?,
        deref: false,
    })
}

/// Why a mutation was not performed.
#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    /// The policy forbids this outright.
    #[error("{reason}")]
    Denied {
        /// The policy's message, ready to show.
        reason: String,
    },
    /// The action needs a typed confirmation and the input was missing or did not match.
    #[error("{op} '{name}': {}; type '{phrase}' to confirm", confirmation_problem(*.given))]
    NotConfirmed {
        /// What was being done.
        op: &'static str,
        /// The ref involved.
        name: String,
        /// The text the user must type.
        phrase: String,
        /// Whether any input was given; empty or whitespace-only input counts as none.
        given: bool,
    },
    /// The branch is checked out, here or in another worktree; deleting it would strand that
    /// worktree's `HEAD`.
    #[error("cannot delete '{name}': {}", checked_out_where(.worktree.as_deref()))]
    CheckedOut {
        /// The branch involved.
        name: String,
        /// The other worktree that has it checked out; `None` for the opened one.
        worktree: Option<String>,
    },
    /// `HEAD` is detached; the operation needs a checked-out branch.
    #[error("cannot {op}: HEAD is detached; check out a branch first")]
    DetachedHead {
        /// What was being done.
        op: &'static str,
    },
    /// The repository moved under the UI between planning and acting.
    #[error("{op}: the repository changed since the list was built; refresh and retry")]
    StaleRepository {
        /// What was being done.
        op: &'static str,
    },
    /// A selected hunk no longer matches the file on disk.
    #[error("'{path}' changed since its hunks were selected; refresh and retry")]
    HunkMismatch {
        /// The file involved.
        path: String,
    },
    /// A hunk selection lists no hunks. A whole-file selection is a different request, so an
    /// empty list is refused rather than read as one.
    #[error("{op}: no hunks selected for '{path}'; select a hunk or the whole file")]
    NoHunksSelected {
        /// What was being done.
        op: &'static str,
        /// The file involved.
        path: String,
    },
    /// A path the operation would write has uncommitted changes.
    #[error("{op}: '{path}' has uncommitted changes; commit, stash, or discard it first")]
    DirtyPath {
        /// What was being done.
        op: &'static str,
        /// The file involved.
        path: String,
    },
    /// No author identity is available: the repository's own config sets no
    /// `user.name`/`user.email` and none was passed in.
    #[error(
        "{op}: no author identity: rc-git reads only this repository's .git/config (not \
         ~/.gitconfig); set user.name/user.email there, or pass an identity"
    )]
    MissingIdentity {
        /// What was being done.
        op: &'static str,
    },
    /// The identity to record has an empty name or email, or one holding `<`, `>` or a line
    /// break, which would corrupt the commit's `author` line. Refused before any object is
    /// written.
    #[error("{op}: author {field} {value:?} {reason}; fix the identity and retry")]
    InvalidIdentity {
        /// What was being done.
        op: &'static str,
        /// `name` or `email`.
        field: &'static str,
        /// The value that was refused.
        value: String,
        /// What is wrong with it, e.g. `is empty`.
        reason: &'static str,
    },
    /// The remote wants credentials rc-git does not have, or rejected the ones given.
    #[error("{op}: '{host}' requires authentication or rejected the credentials")]
    AuthRequired {
        /// What was being done.
        op: &'static str,
        /// The host to authenticate with.
        host: String,
    },
    /// The remote redirected to another host, which asked for credentials. rc-git sends
    /// credentials only to the remote's own host, so no credentials can fix it.
    #[error(
        "{op}: '{host}' redirected to '{requested}', which asked for credentials; rc-git sends \
         credentials only to '{host}' — point the remote's URL at '{requested}' if it is trusted"
    )]
    CredentialHostRefused {
        /// What was being done.
        op: &'static str,
        /// The remote's own host, the only one credentials go to.
        host: String,
        /// The host that asked for credentials.
        requested: String,
    },
    /// The remote's URL scheme is one rc-git does not speak (`git://`, `file://`,
    /// non-loopback `http://`).
    #[error(
        "{op}: remote '{name}' uses '{scheme}'; rc-git only supports https:// and ssh:// remotes"
    )]
    UnsupportedRemote {
        /// What was being done.
        op: &'static str,
        /// The remote involved.
        name: String,
        /// The URL scheme that was refused.
        scheme: String,
    },
    /// The SSH server's host key is not in `known_hosts`; the user must verify and
    /// approve its fingerprint before rc-git will talk to the host.
    #[error(
        "{op}: '{host}' is not a known host; its {key_type} key fingerprint is {fingerprint}. Verify it before trusting."
    )]
    UnknownHostKey {
        /// What was being done.
        op: &'static str,
        /// The host whose key is unknown.
        host: String,
        /// The key's algorithm, e.g. `ssh-ed25519`.
        key_type: String,
        /// The key's fingerprint, `SHA256:...`.
        fingerprint: String,
    },
    /// The SSH server offered a key that contradicts `known_hosts`, or the fingerprint the
    /// user approved earlier for a host `known_hosts` lacks. Never overridable inside rc-git.
    #[error(
        "{op}: the {key_type} host key for '{host}' does not match {} (offered {fingerprint}); this may be a man-in-the-middle — investigate with ssh directly",
        contradicted(.approved.as_deref())
    )]
    HostKeyMismatch {
        /// What was being done.
        op: &'static str,
        /// The host whose key changed.
        host: String,
        /// The offered key's algorithm.
        key_type: String,
        /// The offered key's fingerprint, `SHA256:...`.
        fingerprint: String,
        /// The approved fingerprint the offered key contradicts; `None` when it contradicts
        /// `known_hosts`.
        approved: Option<String>,
    },
    /// A `known_hosts` file revokes the SSH server's key with an `@revoked` line. Never
    /// overridable inside rc-git.
    #[error(
        "{op}: the {key_type} host key for '{host}' ({fingerprint}) is revoked in known_hosts; this may be a man-in-the-middle — investigate with ssh directly"
    )]
    HostKeyRevoked {
        /// What was being done.
        op: &'static str,
        /// The host whose key is revoked.
        host: String,
        /// The offered key's algorithm.
        key_type: String,
        /// The offered key's fingerprint, `SHA256:...`.
        fingerprint: String,
    },
    /// An SSH private key is encrypted and no (or the wrong) passphrase was supplied.
    #[error(
        "{op}: the key '{key}' for '{host}' is encrypted or the passphrase is wrong; enter \
         its passphrase"
    )]
    PassphraseRequired {
        /// What was being done.
        op: &'static str,
        /// The host being authenticated with.
        host: String,
        /// Path of the encrypted key file.
        key: String,
    },
    /// The ssh-agent refused to sign with one of its keys: an `ssh-add -c` key with no askpass,
    /// a dismissed approval prompt, an untouched hardware key. The connection cannot go on to
    /// the next key, so authentication stops here.
    #[error(
        "{op}: the ssh agent at '{agent}' refused to sign with {key} for '{host}'; approve \
         the request in the agent, or remove the key from it"
    )]
    AgentSignRefused {
        /// What was being done.
        op: &'static str,
        /// The host being authenticated with.
        host: String,
        /// The agent key, as its comment and `SHA256:...` fingerprint.
        key: String,
        /// The agent's socket path.
        agent: String,
    },
    /// No remote resolves for the branch: `branch.<name>.remote` is unset and there is no
    /// `origin`, or the remote has no URL.
    #[error(
        "{op}: branch '{branch}' has no remote to use; add an 'origin' remote or set \
         branch.{branch}.remote"
    )]
    NoUpstream {
        /// What was being done.
        op: &'static str,
        /// The branch involved.
        branch: String,
    },
    /// `branch.<name>.merge` names a remote branch whose tracking ref no longer exists:
    /// the branch was deleted on the remote and pruned here. Nothing is pushed, so a
    /// branch the team deleted is not re-created.
    #[error(
        "{op}: upstream '{upstream_ref}' of branch '{branch}' no longer exists; unset \
         branch.{branch}.merge to publish it again"
    )]
    UpstreamGone {
        /// What was being done.
        op: &'static str,
        /// The branch involved.
        branch: String,
        /// Full name of the missing remote-tracking ref.
        upstream_ref: String,
    },
    /// Local and upstream have both moved; a fast-forward is impossible.
    #[error(
        "cannot fast-forward '{branch}': local is {ahead} ahead and {behind} behind; reconcile with git"
    )]
    Diverged {
        /// The branch involved.
        branch: String,
        /// Commits only the local branch has.
        ahead: usize,
        /// Commits only the upstream has.
        behind: usize,
    },
    /// The remote branch moved past the last fetch; a plain push would lose its commits.
    #[error(
        "push rejected: '{branch}' on the remote has commits you don't have; fetch and pull first"
    )]
    PushRejected {
        /// The branch involved.
        branch: String,
    },
    /// The user cancelled before or during the transfer.
    #[error("{op}: cancelled")]
    Cancelled {
        /// What was being done.
        op: &'static str,
    },
    /// The underlying git operation failed. The message carries the whole source chain, each
    /// cause once, so a nested cause (an io error under a gix one) is not lost.
    #[error("{op} '{name}': {}", source_chain(.source.as_ref()))]
    Git {
        /// What was being done.
        op: &'static str,
        /// The ref or revision involved.
        name: String,
        /// The underlying cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

/// `source` and every cause beneath it joined with `: `, skipping a cause whose text the line
/// already holds (many errors repeat their source in their own message). Each is formatted
/// with `{:#}`, which keeps gix from appending its source location.
fn source_chain(source: &(dyn std::error::Error + 'static)) -> String {
    let mut line = format!("{source:#}");
    let mut next = source.source();
    while let Some(cause) = next {
        let text = format!("{cause:#}");
        if !line.contains(&text) {
            line.push_str(": ");
            line.push_str(&text);
        }
        next = cause.source();
    }
    line
}

/// What a refused host key contradicts, for the `HostKeyMismatch` message.
fn contradicted(approved: Option<&str>) -> String {
    match approved {
        Some(approved) => format!("the key approved earlier ({approved})"),
        None => "known_hosts".to_owned(),
    }
}

impl ActionError {
    /// A [`ActionError::Git`] for `op` on `name`, caused by `source`.
    pub(crate) fn git(
        op: &'static str,
        name: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    ) -> Self {
        Self::Git {
            op,
            name: name.into(),
            source: source.into(),
        }
    }
}

/// A `map_err` adapter turning any error into [`ActionError::Git`] for `op` on `name`. The
/// name is copied only when an error actually occurs.
pub(crate) fn git_err<E>(op: &'static str, name: &str) -> impl Fn(E) -> ActionError + Copy
where
    E: std::error::Error + Send + Sync + 'static,
{
    move |source| ActionError::git(op, name, source)
}

/// The commit `HEAD` resolves to; `None` only on an unborn branch. A `HEAD` that cannot be
/// read or peeled (a missing commit, a broken ref) is an error, never mistaken for unborn.
pub(crate) fn head_commit(
    repo: &gix::Repository,
    op: &'static str,
) -> ActionResult<Option<gix::ObjectId>> {
    let head = repo.head().map_err(git_err(op, "HEAD"))?;
    let commit = head.try_into_peeled_id().map_err(git_err(op, "HEAD"))?;
    Ok(commit.map(gix::Id::detach))
}

/// Result alias for this module.
pub type ActionResult<T> = Result<T, ActionError>;

/// What a completed deletion left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    /// Full name of the deleted ref.
    pub full_name: String,
    /// The backup ref written before deletion, when the challenge required one.
    pub backup_ref: Option<String>,
}

/// The decision for deleting a branch, with the facts the policy needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDeletePlan {
    /// Whether every commit of the branch is reachable from another local branch, or from
    /// `HEAD` when `HEAD` is not on the branch itself.
    pub merged: bool,
    /// What the front end must collect.
    pub decision: Decision,
}

/// What deleting the tag `name` would need. Pure: tags are always destructive to delete,
/// so no repository read is required.
#[must_use]
pub fn plan_tag_delete(policy: &Policy, name: &str) -> Decision {
    policy.evaluate(&Action::DeleteTag {
        name: name.to_owned(),
    })
}

/// What committing `files` selected files would need. Pure.
#[must_use]
pub fn plan_commit(policy: &Policy, files: usize) -> Decision {
    policy.evaluate(&Action::Commit { files })
}

/// What stashing `files` selected files would need. Pure.
#[must_use]
pub fn plan_stash_push(policy: &Policy, files: usize) -> Decision {
    policy.evaluate(&Action::StashPush { files })
}

/// What applying a stash entry would need. Pure.
#[must_use]
pub fn plan_stash_apply(policy: &Policy) -> Decision {
    policy.evaluate(&Action::StashApply)
}

/// What popping `stash@{index}`, whose reflog message is `message`, would need. Pure.
#[must_use]
pub fn plan_stash_pop(policy: &Policy, index: usize, message: &str) -> Decision {
    policy.evaluate(&Action::StashPop {
        name: stash::stash_label(index, message),
    })
}

/// What dropping `stash@{index}`, whose reflog message is `message`, would need. Pure.
#[must_use]
pub fn plan_stash_drop(policy: &Policy, index: usize, message: &str) -> Decision {
    policy.evaluate(&Action::StashDrop {
        name: stash::stash_label(index, message),
    })
}

/// What discarding the changes in `files` files would need. Pure.
#[must_use]
pub fn plan_discard(policy: &Policy, files: usize) -> Decision {
    policy.evaluate(&Action::DiscardWorkingChanges { files })
}

/// Creates a local branch at `revspec`, refusing to overwrite an existing one. Creation
/// loses nothing, so the policy allows it without a challenge.
///
/// # Errors
/// [`ActionError::Git`] when `name` is `HEAD` or starts with `-` (names git refuses), the
/// revision does not resolve or does not peel to a commit, or the branch exists.
pub fn create_branch(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    revspec: &str,
) -> ActionResult<()> {
    refuse_reserved_name("create branch", name)?;
    let action = Action::CreateLocalBranch {
        name: name.to_owned(),
    };
    let proof = authorize(policy, &action, "create branch", name, None)?;
    create_ref(
        repo_path,
        &proof,
        "create branch",
        &format!("refs/heads/{name}"),
        revspec,
    )
}

/// Creates a lightweight tag at `revspec`, refusing to overwrite an existing one.
///
/// # Errors
/// [`ActionError::Git`] when `name` is `HEAD` or starts with `-` (names git refuses), the
/// revision does not resolve, or the tag exists.
pub fn create_tag(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    revspec: &str,
) -> ActionResult<()> {
    refuse_reserved_name("create tag", name)?;
    let action = Action::CreateTag {
        name: name.to_owned(),
    };
    let proof = authorize(policy, &action, "create tag", name, None)?;
    create_ref(
        repo_path,
        &proof,
        "create tag",
        &format!("refs/tags/{name}"),
        revspec,
    )
}

/// Refuses a ref name git refuses: `HEAD` would make every `HEAD` lookup ambiguous, and a
/// leading `-` would read as an option on its command line.
fn refuse_reserved_name(op: &'static str, name: &str) -> ActionResult<()> {
    if name == "HEAD" {
        return Err(ActionError::git(
            op,
            name,
            "'HEAD' is not a valid name; git reserves it",
        ));
    }
    if name.starts_with('-') {
        return Err(ActionError::git(
            op,
            name,
            "a name may not start with '-'; git would read it as an option",
        ));
    }
    Ok(())
}

/// What deleting `name` would need: the merged-ness the policy keys on, and its decision.
/// A branch checked out in any worktree, which the policy would not already deny, gets a
/// `Deny` naming the checkout, since the delete would refuse it.
///
/// # Errors
/// [`ActionError::Git`] when the repository or branch cannot be read.
pub fn plan_branch_delete(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
) -> ActionResult<BranchDeletePlan> {
    const OP: &str = "delete branch";
    let repo = open(repo_path, OP, name)?;
    let full_name = format!("refs/heads/{name}");
    require_listed_ref(&repo, OP, &full_name)?;
    let (merged, _) = branch_is_merged(&repo, name)?;
    let decision = match policy.evaluate(&Action::DeleteLocalBranch {
        name: name.to_owned(),
        merged,
    }) {
        Decision::Deny { reason } => Decision::Deny { reason },
        decision => match refuse_checked_out(&repo, OP, name, &full_name) {
            Ok(()) => decision,
            Err(err @ ActionError::CheckedOut { .. }) => Decision::Deny {
                reason: err.to_string(),
            },
            Err(err) => return Err(err),
        },
    };
    Ok(BranchDeletePlan { merged, decision })
}

/// Refuses deleting `name` while any worktree has it checked out: the opened one, or another
/// worktree of the same repository.
///
/// # Errors
/// [`ActionError::CheckedOut`] naming the worktree that holds the branch,
/// [`ActionError::Git`] naming `op` when a worktree's `HEAD` cannot be read.
fn refuse_checked_out(
    repo: &gix::Repository,
    op: &'static str,
    name: &str,
    full_name: &str,
) -> ActionResult<()> {
    let worktree = if head_is_on(repo, op, full_name)? {
        None
    } else if let Some(path) = branch_checked_out_at(repo, op, full_name)? {
        Some(path)
    } else {
        return Ok(());
    };
    Err(ActionError::CheckedOut {
        name: name.to_owned(),
        worktree,
    })
}

/// The path of a worktree other than the opened one whose `HEAD` names `full_name`: the main
/// worktree when the opened one is linked (unless the main repository is bare), or any linked
/// worktree. Compared ignoring case, as [`head_is_on`] does. Fails closed: a worktree whose
/// `HEAD` cannot be read refuses rather than counts as elsewhere.
///
/// # Errors
/// [`ActionError::Git`] naming `op` when the worktree list, a worktree repository, its `HEAD`,
/// or its location cannot be read.
pub(crate) fn branch_checked_out_at(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
) -> ActionResult<Option<String>> {
    let linked = repo.kind() == gix::repository::Kind::LinkedWorkTree;
    let own_id = linked.then(|| repo.git_dir().file_name()).flatten();
    if linked {
        let main = repo.main_repo().map_err(git_err(op, "main worktree"))?;
        if !main.is_bare()
            && head_is_on(&main, op, full_name)?
            && let Some(path) = main.workdir()
        {
            return Ok(Some(path.display().to_string()));
        }
    }
    let proxies = repo.worktrees().map_err(git_err(op, "worktrees"))?;
    for proxy in proxies {
        if own_id.is_some() && proxy.git_dir().file_name() == own_id {
            continue;
        }
        let git_dir = proxy.git_dir().display().to_string();
        let base = proxy.base();
        let other = proxy
            .into_repo_with_possibly_inaccessible_worktree()
            .map_err(git_err(op, &git_dir))?;
        if head_is_on(&other, op, full_name)? {
            let path = base.map_err(git_err(op, &git_dir))?;
            return Ok(Some(path.display().to_string()));
        }
    }
    Ok(None)
}

/// Where a branch that cannot be deleted is checked out.
fn checked_out_where(worktree: Option<&str>) -> String {
    match worktree {
        Some(path) => format!("it is checked out in the worktree at '{path}'"),
        None => "it is the checked-out branch".to_owned(),
    }
}

/// Whether `HEAD`'s symbolic chain (`HEAD -> alias -> branch`) passes through `full_name`,
/// compared ignoring case so a case variant cannot strand `HEAD` on a case-folding volume. A
/// detached `HEAD`, or a chain ending at a missing ref, reaches nothing. A chain longer than
/// git's own symref depth limit (a cycle included) counts as reaching it, so every caller
/// fails closed: it refuses the delete, or stops counting `HEAD` as other evidence.
///
/// # Errors
/// [`ActionError::Git`] naming `op` when `HEAD` or a ref on its chain cannot be read.
fn head_is_on(repo: &gix::Repository, op: &'static str, full_name: &str) -> ActionResult<bool> {
    const MAX_SYMREF_DEPTH: usize = 5;
    let mut reference = repo.find_reference("HEAD").map_err(git_err(op, "HEAD"))?;
    for _ in 0..MAX_SYMREF_DEPTH {
        let Some(target) = reference.target().try_name().map(ToOwned::to_owned) else {
            return Ok(false);
        };
        if target.as_bstr().eq_ignore_ascii_case(full_name.as_bytes()) {
            return Ok(true);
        }
        match repo.try_find_reference(target.as_ref()) {
            Ok(Some(next)) => reference = next,
            Ok(None) => return Ok(false),
            Err(err) => return Err(git_err(op, "HEAD")(err)),
        }
    }
    Ok(true)
}

/// Deletes a local branch. `input` answers the challenge when the policy demands one; pass
/// what the user typed, or `None` when the decision was a plain confirm. A destructive
/// deletion writes `refs/rc-git/backup/branch/<name>` first and reports it.
///
/// # Errors
/// [`ActionError::Denied`] for protected branches, [`ActionError::CheckedOut`] for a branch the
/// `HEAD` of this or another worktree is on, [`ActionError::NotConfirmed`] when the typed input
/// does not match.
pub fn delete_branch(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "delete branch";
    let repo = open(repo_path, OP, name)?;
    let full_name = format!("refs/heads/{name}");
    require_listed_ref(&repo, OP, &full_name)?;
    refuse_checked_out(&repo, OP, name, &full_name)?;
    let (merged, judged_tip) = branch_is_merged(&repo, name)?;
    let action = Action::DeleteLocalBranch {
        name: name.to_owned(),
        merged,
    };
    let proof = authorize(policy, &action, OP, name, input)?;
    delete_ref(
        &Writer::new(&repo, &proof),
        OP,
        &full_name,
        ("branch", name),
        Some(judged_tip),
    )
}

/// Deletes a tag. Always destructive under the policy: `input` must match the tag name.
/// `expected` is the object the caller showed the tag pointing at, after peeling (the ref
/// list's target); a tag that peels to anything else is not deleted. Writes
/// `refs/rc-git/backup/tag/<name>` first and reports it.
///
/// # Errors
/// [`ActionError::StaleRepository`] when the tag no longer peels to `expected` or moves before
/// the delete lands, [`ActionError::NotConfirmed`] when the typed input does not match,
/// [`ActionError::Git`] when the tag does not exist.
pub fn delete_tag(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    expected: rc_git_core::Oid,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "delete tag";
    let repo = open(repo_path, OP, name)?;
    let full_name = format!("refs/tags/{name}");
    require_listed_ref(&repo, OP, &full_name)?;
    let judged_tip = tag_as_shown(&repo, OP, &full_name, expected)?;
    let action = Action::DeleteTag {
        name: name.to_owned(),
    };
    let proof = authorize(policy, &action, OP, name, input)?;
    delete_ref(
        &Writer::new(&repo, &proof),
        OP,
        &full_name,
        ("tag", name),
        Some(judged_tip),
    )
}

/// The id [`delete_ref`] judges the tag `full_name` by (its own target, the tag object for an
/// annotated tag), once the object it peels to is checked against `expected`.
///
/// # Errors
/// [`ActionError::StaleRepository`] naming `op` when the tag peels to another object,
/// [`ActionError::Git`] when it cannot be read or peeled.
fn tag_as_shown(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
    expected: rc_git_core::Oid,
) -> ActionResult<gix::ObjectId> {
    let reference = repo
        .find_reference(full_name)
        .map_err(git_err(op, full_name))?;
    let direct = reference.target().try_id().map(ToOwned::to_owned);
    let peeled = reference
        .into_fully_peeled_id()
        .map_err(git_err(op, full_name))?
        .detach();
    if peeled.as_slice() != expected.as_bytes() {
        return Err(ActionError::StaleRepository { op });
    }
    Ok(direct.unwrap_or(peeled))
}

/// Asks `policy` about `action` and turns the user's `input` into the proof every write
/// needs, or the matching error naming `op` and `name`.
///
/// # Errors
/// [`ActionError::Denied`] when the policy forbids the action, [`ActionError::NotConfirmed`]
/// when the input does not answer its challenge.
fn authorize(
    policy: &Policy,
    action: &Action,
    op: &'static str,
    name: &str,
    input: Option<&str>,
) -> ActionResult<Confirmation> {
    policy
        .authorize(action, input)
        .map_err(|refusal| match refusal {
            Refusal::Denied(reason) => ActionError::Denied { reason },
            Refusal::NotConfirmed { phrase } => ActionError::NotConfirmed {
                op,
                name: name.to_owned(),
                phrase,
                given: input.is_some_and(|typed| !typed.trim().is_empty()),
            },
        })
}

fn confirmation_problem(given: bool) -> &'static str {
    if given {
        "the typed confirmation did not match"
    } else {
        "no typed confirmation was given"
    }
}

/// The object cache every guard handle gets: a merged check, tree plan or diff re-reads the
/// same commits and trees, and an uncached handle decodes each one again per read.
const OBJECT_CACHE_BYTES: usize = 4 * 1024 * 1024;
/// The delta-base cache every guard handle gets, the same bound core gives its handles.
const PACK_CACHE_BYTES: usize = 32 * 1024 * 1024;

fn open(repo_path: &Path, op: &'static str, name: &str) -> ActionResult<gix::Repository> {
    let mut repo = gix::ThreadSafeRepository::open_opts(repo_path, gix::open::Options::isolated())
        .map(|shared| shared.to_thread_local())
        .map_err(git_err(op, name))?;
    // Set unconditionally: the repository's own config (`gitoxide.objects.cacheLimit`,
    // `core.deltaBaseCacheLimit`) would otherwise size both caches, up to unbounded.
    repo.object_cache_size(OBJECT_CACHE_BYTES);
    repo.objects.set_pack_cache(|| {
        Box::new(gix::odb::pack::cache::lru::MemoryCappedHashmap::new(
            PACK_CACHE_BYTES,
        ))
    });
    rc_git_core::require_files_ref_storage(
        repo_path,
        repo.config_snapshot()
            .string("extensions.refStorage")
            .as_deref()
            .map(Vec::as_slice),
    )
    .map_err(git_err(op, name))?;
    rc_git_core::require_work_tree_at(
        repo_path,
        repo.git_dir(),
        repo.workdir(),
        repo.config_snapshot()
            .string("core.worktree")
            .as_deref()
            .map(Vec::as_slice),
    )
    .map_err(git_err(op, name))?;
    Ok(repo)
}

/// Whether a bisect may stay in progress while `op` runs. A commit records work on the bisected
/// commit as git allows; anything that moves `HEAD` would desynchronize the bisect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bisect {
    Allowed,
    Refused,
}

/// Refuses `op` while a merge, cherry-pick, revert, rebase or mailbox apply is in progress:
/// its pending state (`MERGE_HEAD`, `CHERRY_PICK_HEAD`, …) would survive the action and be
/// finished on top of it. A bisect in progress is allowed. Errors name `op` and `target`.
///
/// # Errors
/// [`ActionError::Git`] naming the operation in progress.
fn refuse_unfinished_operation(
    repo: &gix::Repository,
    op: &'static str,
    target: &str,
) -> ActionResult<()> {
    refuse_in_progress(repo, op, target, Bisect::Allowed)
}

/// Refuses `op`, which moves `HEAD` to another commit, while any git operation is in progress,
/// a bisect included. Errors name `op` and `target`.
///
/// # Errors
/// [`ActionError::Git`] naming the operation in progress.
fn refuse_any_operation_in_progress(
    repo: &gix::Repository,
    op: &'static str,
    target: &str,
) -> ActionResult<()> {
    refuse_in_progress(repo, op, target, Bisect::Refused)
}

fn refuse_in_progress(
    repo: &gix::Repository,
    op: &'static str,
    target: &str,
    bisect: Bisect,
) -> ActionResult<()> {
    use gix::state::InProgress;
    let name = match repo.state() {
        None => return Ok(()),
        Some(InProgress::Bisect) if bisect == Bisect::Allowed => return Ok(()),
        Some(InProgress::Bisect) => "bisect",
        Some(InProgress::Merge) => "merge",
        Some(InProgress::CherryPick | InProgress::CherryPickSequence) => "cherry-pick",
        Some(InProgress::Revert | InProgress::RevertSequence) => "revert",
        Some(
            InProgress::Rebase | InProgress::RebaseInteractive | InProgress::ApplyMailboxRebase,
        ) => "rebase",
        Some(InProgress::ApplyMailbox) => "mailbox apply (git am)",
    };
    Err(ActionError::git(
        op,
        target,
        format!("a {name} is in progress; finish or abort it in git first"),
    ))
}

fn create_ref(
    repo_path: &Path,
    proof: &Confirmation,
    op: &'static str,
    full_name: &str,
    revspec: &str,
) -> ActionResult<()> {
    let repo = open(repo_path, op, full_name)?;
    refuse_case_variant(&repo, op, full_name)?;
    refuse_existing(&repo, op, full_name)?;
    let target = resolve_ref_target(&repo, op, full_name, revspec)?;
    let edit = update_edit(
        full_name,
        PreviousValue::MustNotExist,
        Target::Object(target),
        &format!("rc-git: {op}"),
        op,
    )?;
    Writer::new(&repo, proof).apply_edit(edit, op, full_name)
}

/// Refuses `full_name` when it already exists. `PreviousValue::MustNotExist` alone lets an edit
/// through when the existing ref already holds the new target, and that edit appends a reflog
/// line; the edit keeps it only as the backstop for a ref created after this read.
fn refuse_existing(repo: &gix::Repository, op: &'static str, full_name: &str) -> ActionResult<()> {
    let existing = repo
        .try_find_reference(full_name)
        .map_err(git_err(op, full_name))?;
    if existing.is_some() {
        return Err(ActionError::git(op, full_name, "it already exists"));
    }
    Ok(())
}

/// The object `revspec` names, for a new ref `full_name`. A branch is peeled to a commit and
/// refused when it does not peel, as `git branch` does; a tag keeps the object as named, as
/// `git tag` does, so a tag of an annotated tag points at the tag object.
fn resolve_ref_target(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
    revspec: &str,
) -> ActionResult<gix::ObjectId> {
    let id = repo
        .rev_parse_single(revspec)
        .map_err(git_err(op, revspec))?;
    if !full_name.starts_with("refs/heads/") {
        return Ok(id.detach());
    }
    let object = id.object().map_err(git_err(op, revspec))?;
    let kind = object.kind;
    let commit = object
        .peel_to_kind(gix::object::Kind::Commit)
        .map_err(|_| {
            ActionError::git(
                op,
                revspec,
                format!("a {kind} is not a valid branch point; name a commit"),
            )
        })?;
    Ok(commit.id)
}

/// Deletes `full_name`, first writing a backup ref under `refs/rc-git/backup/<kind>/<name>`
/// for the `(kind, name)` given when the answered challenge asked for one. `judged_tip` is the
/// commit the policy decision was made on; a ref that no longer peels to it, or that moves
/// before the transaction lands, is not deleted.
///
/// # Errors
/// [`ActionError::StaleRepository`] naming `op` when the ref moved since it was judged,
/// [`ActionError::Git`] when it cannot be read or the transaction fails.
fn delete_ref(
    writer: &Writer<'_>,
    op: &'static str,
    full_name: &str,
    (backup_kind, short_name): (&str, &str),
    judged_tip: Option<gix::ObjectId>,
) -> ActionResult<Deleted> {
    let reference = writer
        .repo()
        .find_reference(full_name)
        .map_err(git_err(op, full_name))?;
    let target = reference.target().into_owned();
    let target_id = match target.clone() {
        Target::Object(id) => id,
        Target::Symbolic(_) => reference
            .clone()
            .into_fully_peeled_id()
            .map_err(git_err(op, full_name))?
            .detach(),
    };
    drop(reference);
    if judged_tip.is_some_and(|tip| tip != target_id) {
        return Err(ActionError::StaleRepository { op });
    }
    let backup = if writer.backup_required() {
        Some(writer.write_backup_ref(backup_kind, short_name, target_id)?)
    } else {
        None
    };
    let edit = delete_edit(
        full_name,
        PreviousValue::MustExistAndMatch(target.clone()),
        op,
    )?;
    if let Err(err) = writer.apply_edit(edit, op, full_name) {
        return Err(if ref_moved(writer.repo(), full_name, &target) {
            ActionError::StaleRepository { op }
        } else {
            err
        });
    }
    Ok(Deleted {
        full_name: full_name.to_owned(),
        backup_ref: backup,
    })
}

/// Whether `full_name` no longer has `expected` as its target, gone included: the reason a
/// `MustExistAndMatch` transaction on it failed. A read error counts as not moved, so the
/// transaction's own error is reported.
fn ref_moved(repo: &gix::Repository, full_name: &str, expected: &Target) -> bool {
    match repo.try_find_reference(full_name) {
        Ok(Some(reference)) => reference.target() != expected.to_ref(),
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Whether every commit of `name` is reachable from another direct local branch or from
/// `HEAD` when `HEAD`'s symbolic chain does not reach `name`, and the tip that answer was
/// judged on. A symbolic branch is skipped: an alias of `name` (or of another branch) is no
/// independent evidence that the commits survive the delete.
fn branch_is_merged(repo: &gix::Repository, name: &str) -> ActionResult<(bool, gix::ObjectId)> {
    const OP: &str = "check branch reachability";
    let full_name = format!("refs/heads/{name}");
    require_listed_ref(repo, OP, &full_name)?;
    let reference = repo
        .find_reference(&full_name)
        .map_err(git_err(OP, &full_name))?;
    let tip = reference
        .into_fully_peeled_id()
        .map_err(git_err(OP, &full_name))?
        .detach();
    let others = other_branch_tips(repo, OP, &full_name)?;
    Ok((is_ancestor_of_any(repo, tip, &others), tip))
}

/// The tips that count as other evidence for `full_name`'s commits: `HEAD` unless its
/// symbolic chain reaches `full_name` or it is unborn, and every direct local branch except
/// `full_name`. A ref that cannot be read is an error, never a missing tip: a skipped tip
/// would turn a merged branch into an unmerged one.
fn other_branch_tips(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
) -> ActionResult<Vec<gix::ObjectId>> {
    let mut others: Vec<gix::ObjectId> = Vec::new();
    if !head_is_on(repo, op, full_name)?
        && let Some(head) = head_commit(repo, op)?
    {
        others.push(head);
    }
    let references = repo.references().map_err(git_err(op, full_name))?;
    let branches = references
        .local_branches()
        .map_err(git_err(op, "refs/heads"))?;
    for branch in branches {
        let branch = branch.map_err(|source| ActionError::Git {
            op,
            name: "refs/heads".into(),
            source,
        })?;
        let symbolic = branch.target().try_name().is_some();
        if symbolic || branch.name().as_bstr() == full_name {
            continue;
        }
        let name = branch.name().as_bstr().to_string();
        let id = branch.into_fully_peeled_id().map_err(git_err(op, &name))?;
        others.push(id.detach());
    }
    Ok(others)
}

/// Whether `ancestor` is reachable from at least one of `tips`, answered with one merge-base
/// walk however many tips there are. No tips means no: the walk would answer `ancestor`
/// itself. A walk that fails reads as unreachable, the answer that asks for more care.
fn is_ancestor_of_any(
    repo: &gix::Repository,
    ancestor: gix::ObjectId,
    tips: &[gix::ObjectId],
) -> bool {
    if tips.is_empty() {
        return false;
    }
    repo.merge_bases_many(ancestor, tips)
        .is_ok_and(|bases| bases.iter().any(|base| base.detach() == ancestor))
}

/// How `full_name` appears in the repository's own ref listing, loose and packed alike.
struct ListedSpellings {
    /// A ref is listed under exactly this name.
    exact: bool,
    /// A ref whose name differs from `full_name` only in ASCII case.
    variant: Option<String>,
}

/// Reads the branch or tag listing `full_name` belongs to. The listing reports names as they
/// are stored, so unlike a lookup it never resolves a name through filesystem case folding.
fn listed_spellings(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
) -> ActionResult<ListedSpellings> {
    let references = repo.references().map_err(git_err(op, full_name))?;
    let listing = if full_name.starts_with("refs/tags/") {
        references.tags()
    } else {
        references.local_branches()
    }
    .map_err(git_err(op, full_name))?;
    let mut spellings = ListedSpellings {
        exact: false,
        variant: None,
    };
    for reference in listing {
        let reference = reference.map_err(|source| ActionError::git(op, full_name, source))?;
        let name = reference.name().as_bstr();
        if name == full_name.as_bytes() {
            spellings.exact = true;
        } else if name.eq_ignore_ascii_case(full_name.as_bytes()) {
            spellings.variant = Some(name.to_string());
        }
    }
    Ok(spellings)
}

/// Whether the repository's own config sets `core.ignoreCase`, as git does on a
/// case-insensitive volume. Read under the isolated open, so only the repository's config.
fn ignores_case(repo: &gix::Repository) -> bool {
    repo.config_snapshot()
        .boolean("core.ignoreCase")
        .unwrap_or(false)
}

/// The refusal for `full_name` when `variant` differs from it only in case.
fn case_clash(op: &'static str, full_name: &str, variant: &str) -> ActionError {
    ActionError::git(
        op,
        full_name,
        format!(
            "'{variant}' differs only in case, and this repository ignores case \
             (core.ignoreCase), so both names are one file; use git directly"
        ),
    )
}

/// Requires a ref listed under exactly `full_name`. A name that reaches a ref only through
/// filesystem case folding (`MAIN` opening `main` on a case-insensitive volume) is not
/// found, and under `core.ignoreCase` a name with a case-variant sibling is refused, since a
/// write to one would land on the other.
///
/// # Errors
/// [`ActionError::Git`] naming `op` and `full_name`.
fn require_listed_ref(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
) -> ActionResult<()> {
    let spellings = listed_spellings(repo, op, full_name)?;
    if let Some(variant) = &spellings.variant
        && ignores_case(repo)
    {
        return Err(case_clash(op, full_name, variant));
    }
    if !spellings.exact {
        return Err(ActionError::git(
            op,
            full_name,
            "no ref has exactly this name",
        ));
    }
    Ok(())
}

/// Under `core.ignoreCase`, refuses creating `full_name` when a ref differing only in case
/// exists: the new loose ref would be the existing one's file.
///
/// # Errors
/// [`ActionError::Git`] naming `op`, `full_name`, and the existing spelling.
fn refuse_case_variant(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
) -> ActionResult<()> {
    if !ignores_case(repo) {
        return Ok(());
    }
    match listed_spellings(repo, op, full_name)?.variant {
        Some(variant) => Err(case_clash(op, full_name, &variant)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wrapper whose message omits its cause, as many gix errors do.
    #[derive(Debug, thiserror::Error)]
    #[error("could not write the object")]
    struct Wrapper(#[source] std::io::Error);

    /// A wrapper that repeats its cause in its own message.
    #[derive(Debug, thiserror::Error)]
    #[error("lock failed: {0}")]
    struct Repeats(#[source] std::io::Error);

    #[test]
    fn a_git_error_prints_every_nested_cause_once() {
        let nested = std::io::Error::other("disk full");
        let err = ActionError::git("commit", "index", Wrapper(nested));
        assert_eq!(
            err.to_string(),
            "commit 'index': could not write the object: disk full"
        );

        let repeated = std::io::Error::other("held by another process");
        let err = ActionError::git("commit", "index", Repeats(repeated));
        assert_eq!(
            err.to_string(),
            "commit 'index': lock failed: held by another process"
        );
    }

    #[test]
    fn a_host_key_mismatch_names_what_it_contradicts() {
        let mismatch = |approved: Option<&str>| ActionError::HostKeyMismatch {
            op: "fetch",
            host: "git.example.com".to_owned(),
            key_type: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:offered".to_owned(),
            approved: approved.map(str::to_owned),
        };
        assert_eq!(
            mismatch(None).to_string(),
            "fetch: the ssh-ed25519 host key for 'git.example.com' does not match known_hosts \
             (offered SHA256:offered); this may be a man-in-the-middle — investigate with ssh \
             directly"
        );
        assert_eq!(
            mismatch(Some("SHA256:approved")).to_string(),
            "fetch: the ssh-ed25519 host key for 'git.example.com' does not match the key \
             approved earlier (SHA256:approved) (offered SHA256:offered); this may be a \
             man-in-the-middle — investigate with ssh directly"
        );
    }

    #[test]
    fn a_revoked_host_key_says_revoked_not_changed() {
        let err = ActionError::HostKeyRevoked {
            op: "fetch",
            host: "git.example.com".to_owned(),
            key_type: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:offered".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "fetch: the ssh-ed25519 host key for 'git.example.com' (SHA256:offered) is revoked \
             in known_hosts; this may be a man-in-the-middle — investigate with ssh directly"
        );
    }

    #[test]
    fn an_invalid_identity_names_the_op_the_field_and_the_value() {
        let err = ActionError::InvalidIdentity {
            op: "commit",
            field: "email",
            value: "b> <c".to_owned(),
            reason: "must not contain '<', '>' or a line break",
        };
        assert_eq!(
            err.to_string(),
            "commit: author email \"b> <c\" must not contain '<', '>' or a line break; fix the \
             identity and retry"
        );
    }

    #[test]
    fn a_refused_credential_host_names_both_hosts() {
        let err = ActionError::CredentialHostRefused {
            op: "fetch",
            host: "git.example.com".to_owned(),
            requested: "login.example.net".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "fetch: 'git.example.com' redirected to 'login.example.net', which asked for \
             credentials; rc-git sends credentials only to 'git.example.com' — point the \
             remote's URL at 'login.example.net' if it is trusted"
        );
    }

    #[test]
    fn a_refused_agent_signature_names_the_agent_the_key_and_the_host() {
        let err = ActionError::AgentSignRefused {
            op: "fetch",
            host: "git.example.com".to_owned(),
            key: "'work@laptop' (SHA256:abc)".to_owned(),
            agent: "/tmp/agent.sock".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "fetch: the ssh agent at '/tmp/agent.sock' refused to sign with 'work@laptop' \
             (SHA256:abc) for 'git.example.com'; approve the request in the agent, or remove \
             the key from it"
        );
    }

    #[test]
    fn deleting_a_branch_that_moved_since_it_was_judged_refuses_as_stale() {
        let fx = rc_git_testkit::Fixture::standard();
        let tip = fx.rev_parse("feature");
        let judged = gix::ObjectId::from_hex(fx.rev_parse("feature~1").as_bytes()).unwrap();
        let repo = gix::open_opts(fx.path(), gix::open::Options::isolated()).unwrap();
        let proof = Policy::default().authorize(&Action::Fetch, None).unwrap();
        let err = delete_ref(
            &Writer::new(&repo, &proof),
            "delete branch",
            "refs/heads/feature",
            ("branch", "feature"),
            Some(judged),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                ActionError::StaleRepository {
                    op: "delete branch"
                }
            ),
            "{err:?}"
        );
        assert_eq!(fx.rev_parse("feature"), tip, "the moved branch is kept");
    }

    #[test]
    fn a_guard_handle_caches_objects() {
        let fx = rc_git_testkit::Fixture::standard();
        let repo = open(fx.path(), "delete branch", "feature").unwrap();
        assert!(repo.objects.has_object_cache());
        assert!(repo.objects.has_pack_cache());
    }

    #[test]
    fn the_repository_config_cannot_size_the_guard_handle_caches() {
        let fx = rc_git_testkit::Fixture::standard();
        fx.git(&["config", "core.deltaBaseCacheLimit", "0"]);
        fx.git(&["config", "gitoxide.objects.cacheLimit", "0"]);
        let repo = open(fx.path(), "delete branch", "feature").unwrap();
        assert_eq!(
            (
                repo.objects.has_object_cache(),
                repo.objects.has_pack_cache()
            ),
            (true, true),
            "guard handle (object cache, delta-base cache)"
        );
    }
}
