//! The one write path in rc-git. Every mutation here first asks [`Policy::evaluate`] and,
//! for a destructive decision, proves the challenge was answered by constructing a
//! [`Confirmation`] through [`Challenge::verify`]. Destructive deletions write a backup ref
//! under `refs/rc-git/backup/` before anything is removed.
//!
//! Repositories open isolated, exactly as the read-only core does: only the repository's own
//! configuration is consulted.

use std::path::Path;

use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};

use crate::{Action, Challenge, Confirmation, Decision, Policy, Risk};

mod checkout;
mod commit;
mod discard;
mod remote;
mod stash;
mod treeapply;
mod treebuild;
mod workfile;

pub use checkout::{CheckoutOutcome, checkout_branch};
pub use commit::{Committed, commit, configured_identity};
pub use discard::{discard_files, discard_hunks};
pub use remote::{
    Cancel, Credentials, FetchOutcome, PullOutcome, PushOutcome, RefUpdate, RemoteAuth,
    SyncOutcome, UpstreamStatus, fetch_upstream, pull_fast_forward, push_upstream, sync,
    upstream_status,
};
pub use stash::{StashSaved, stash_apply, stash_drop, stash_pop, stash_push};

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

/// Applies one ref edit as the synthetic `rc-git` committer, so reflog entries never depend
/// on user configuration the isolated open refuses to read.
fn apply_edit(
    repo: &gix::Repository,
    edit: RefEdit,
    op: &'static str,
    name: &str,
) -> ActionResult<()> {
    apply_edit_as(repo, edit, op, name, REFLOG_NAME, REFLOG_EMAIL)
}

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

/// Applies one ref edit with an explicit reflog committer. The stash uses the user's own
/// identity here because `git stash list` shows the reflog's author; everything else goes
/// through [`apply_edit`].
fn apply_edit_as(
    repo: &gix::Repository,
    edit: RefEdit,
    op: &'static str,
    name: &str,
    committer_name: &str,
    committer_email: &str,
) -> ActionResult<()> {
    let time = gix::date::Time::now_local_or_utc()
        .format(gix::date::time::Format::Raw)
        .unwrap_or_else(|_| "0 +0000".to_owned());
    let committer = gix::actor::SignatureRef {
        name: committer_name.into(),
        email: committer_email.into(),
        time: &time,
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    repo.edit_references_as(Some(edit), Some(committer))
        .map_err(git_err(op, name))?;
    Ok(())
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
    /// The action needs a typed confirmation and the input did not match.
    #[error("{op} '{name}': the typed confirmation did not match")]
    NotConfirmed {
        /// What was being done.
        op: &'static str,
        /// The ref involved.
        name: String,
    },
    /// The branch is checked out; deleting it would strand `HEAD`.
    #[error("cannot delete '{name}': it is the checked-out branch")]
    CheckedOut {
        /// The branch involved.
        name: String,
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
        "no author identity: rc-git reads only this repository's .git/config (not \
         ~/.gitconfig); set user.name/user.email there, or pass an identity"
    )]
    MissingIdentity,
    /// The remote wants credentials rc-git does not have, or rejected the ones given.
    #[error("{op}: '{host}' requires authentication or rejected the credentials")]
    AuthRequired {
        /// What was being done.
        op: &'static str,
        /// The host to authenticate with.
        host: String,
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
    /// The SSH server offered a key that contradicts `known_hosts`. Never overridable
    /// inside rc-git.
    #[error(
        "{op}: the {key_type} host key for '{host}' does not match known_hosts (offered {fingerprint}); this may be a man-in-the-middle — investigate with ssh directly"
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
    },
    /// An SSH private key is encrypted and no (or the wrong) passphrase was supplied.
    #[error("{op}: the key '{key}' for '{host}' is encrypted; enter its passphrase")]
    PassphraseRequired {
        /// What was being done.
        op: &'static str,
        /// The host being authenticated with.
        host: String,
        /// Path of the encrypted key file.
        key: String,
    },
    /// The branch has no configured upstream remote.
    #[error("{op}: branch '{branch}' has no upstream; configure branch.{branch}.remote first")]
    NoUpstream {
        /// What was being done.
        op: &'static str,
        /// The branch involved.
        branch: String,
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
    /// The underlying git operation failed.
    #[error("{op} '{name}': {source:#}")]
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
    /// Whether every commit of the branch is reachable from another local branch or `HEAD`.
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

/// Creates a local branch at `revspec`, refusing to overwrite an existing one. Creation
/// loses nothing, so the policy allows it without a challenge.
///
/// # Errors
/// [`ActionError::Git`] when the revision does not resolve or the branch exists.
pub fn create_branch(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    revspec: &str,
) -> ActionResult<()> {
    let decision = policy.evaluate(&Action::CreateLocalBranch {
        name: name.to_owned(),
    });
    let _proof = confirm(decision, "create branch", name, None)?;
    create_ref(
        repo_path,
        "create branch",
        &format!("refs/heads/{name}"),
        revspec,
    )
}

/// Creates a lightweight tag at `revspec`, refusing to overwrite an existing one.
///
/// # Errors
/// [`ActionError::Git`] when the revision does not resolve or the tag exists.
pub fn create_tag(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    revspec: &str,
) -> ActionResult<()> {
    let decision = policy.evaluate(&Action::CreateTag {
        name: name.to_owned(),
    });
    let _proof = confirm(decision, "create tag", name, None)?;
    create_ref(
        repo_path,
        "create tag",
        &format!("refs/tags/{name}"),
        revspec,
    )
}

/// What deleting `name` would need: the merged-ness the policy keys on, and its decision.
///
/// # Errors
/// [`ActionError::Git`] when the repository or branch cannot be read.
pub fn plan_branch_delete(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
) -> ActionResult<BranchDeletePlan> {
    let repo = open(repo_path, "delete branch", name)?;
    let merged = branch_is_merged(&repo, name)?;
    let decision = policy.evaluate(&Action::DeleteLocalBranch {
        name: name.to_owned(),
        merged,
    });
    Ok(BranchDeletePlan { merged, decision })
}

/// Deletes a local branch. `input` answers the challenge when the policy demands one; pass
/// what the user typed, or `None` when the decision was a plain confirm. A destructive
/// deletion writes `refs/rc-git/backup/branch/<name>` first and reports it.
///
/// # Errors
/// [`ActionError::Denied`] for protected branches, [`ActionError::CheckedOut`] for the
/// branch `HEAD` is on, [`ActionError::NotConfirmed`] when the typed input does not match.
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
    if let Ok(Some(head_name)) = repo.head_name()
        && head_name
            .as_bstr()
            .eq_ignore_ascii_case(full_name.as_bytes())
    {
        return Err(ActionError::CheckedOut {
            name: name.to_owned(),
        });
    }
    let merged = branch_is_merged(&repo, name)?;
    let decision = policy.evaluate(&Action::DeleteLocalBranch {
        name: name.to_owned(),
        merged,
    });
    let confirmation = confirm(decision, OP, name, input)?;
    delete_ref(&repo, OP, &full_name, "branch", name, confirmation)
}

/// Deletes a tag. Always destructive under the policy: `input` must match the tag name.
/// Writes `refs/rc-git/backup/tag/<name>` first and reports it.
///
/// # Errors
/// [`ActionError::NotConfirmed`] when the typed input does not match,
/// [`ActionError::Git`] when the tag does not exist.
pub fn delete_tag(
    repo_path: &Path,
    policy: &Policy,
    name: &str,
    input: Option<&str>,
) -> ActionResult<Deleted> {
    const OP: &str = "delete tag";
    let repo = open(repo_path, OP, name)?;
    let full_name = format!("refs/tags/{name}");
    require_listed_ref(&repo, OP, &full_name)?;
    let decision = policy.evaluate(&Action::DeleteTag {
        name: name.to_owned(),
    });
    let confirmation = confirm(decision, OP, name, input)?;
    delete_ref(&repo, OP, &full_name, "tag", name, confirmation)
}

/// Turns a decision plus the user's input into proof, or the matching error.
fn confirm(
    decision: Decision,
    op: &'static str,
    name: &str,
    input: Option<&str>,
) -> ActionResult<(Challenge, Confirmation)> {
    match decision {
        Decision::Allow => {
            // Allow never happens for deletions today; treat it as a no-phrase challenge so
            // the write path still owns a Confirmation.
            let challenge = Challenge {
                risk: Risk::Safe,
                summary: String::new(),
                typed_phrase: None,
                backup_ref: false,
            };
            let confirmation = challenge.verify("").ok_or(ActionError::NotConfirmed {
                op,
                name: name.to_owned(),
            })?;
            Ok((challenge, confirmation))
        }
        Decision::Confirm(challenge) => {
            let confirmation =
                challenge
                    .verify(input.unwrap_or(""))
                    .ok_or(ActionError::NotConfirmed {
                        op,
                        name: name.to_owned(),
                    })?;
            Ok((challenge, confirmation))
        }
        Decision::Deny { reason } => Err(ActionError::Denied { reason }),
    }
}

fn open(repo_path: &Path, op: &'static str, name: &str) -> ActionResult<gix::Repository> {
    let repo = gix::ThreadSafeRepository::open_opts(repo_path, gix::open::Options::isolated())
        .map(|shared| shared.to_thread_local())
        .map_err(git_err(op, name))?;
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

fn create_ref(
    repo_path: &Path,
    op: &'static str,
    full_name: &str,
    revspec: &str,
) -> ActionResult<()> {
    let repo = open(repo_path, op, full_name)?;
    refuse_case_variant(&repo, op, full_name)?;
    let target = repo
        .rev_parse_single(revspec)
        .map_err(git_err(op, revspec))?
        .detach();
    let edit = update_edit(
        full_name,
        PreviousValue::MustNotExist,
        Target::Object(target),
        &format!("rc-git: {op}"),
        op,
    )?;
    apply_edit(&repo, edit, op, full_name)
}

fn delete_ref(
    repo: &gix::Repository,
    op: &'static str,
    full_name: &str,
    backup_kind: &str,
    short_name: &str,
    (challenge, _confirmation): (Challenge, Confirmation),
) -> ActionResult<Deleted> {
    let reference = repo
        .find_reference(full_name)
        .map_err(git_err(op, full_name))?;
    let target = reference.target().into_owned();
    let backup = if challenge.backup_ref {
        let target_id = match target.clone() {
            Target::Object(id) => id,
            Target::Symbolic(_) => reference
                .clone()
                .into_fully_peeled_id()
                .map_err(git_err(op, full_name))?
                .detach(),
        };
        Some(write_backup_ref(repo, backup_kind, short_name, target_id)?)
    } else {
        None
    };
    drop(reference);
    let edit = delete_edit(full_name, PreviousValue::MustExistAndMatch(target), op)?;
    apply_edit(repo, edit, op, full_name)?;
    Ok(Deleted {
        full_name: full_name.to_owned(),
        backup_ref: backup,
    })
}

/// Writes `refs/rc-git/backup/<kind>/<name>` pointing at `target`, appending `-2`, `-3`, …
/// when earlier backups of the same name exist, so nothing is ever overwritten.
fn write_backup_ref(
    repo: &gix::Repository,
    kind: &str,
    name: &str,
    target: gix::ObjectId,
) -> ActionResult<String> {
    const OP: &str = "write backup ref";
    for attempt in 1..=1000_u32 {
        let candidate = if attempt == 1 {
            format!("refs/rc-git/backup/{kind}/{name}")
        } else {
            format!("refs/rc-git/backup/{kind}/{name}-{attempt}")
        };
        let edit = update_edit(
            &candidate,
            PreviousValue::MustNotExist,
            Target::Object(target),
            "rc-git: backup before deletion",
            OP,
        )?;
        match apply_edit(repo, edit, OP, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) => {
                if attempt == 1000 {
                    return Err(error);
                }
            }
        }
    }
    unreachable!("the loop always returns")
}

/// Whether every commit of `name` is reachable from another local branch or `HEAD`.
fn branch_is_merged(repo: &gix::Repository, name: &str) -> ActionResult<bool> {
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
    let mut others: Vec<gix::ObjectId> = Vec::new();
    if let Ok(head) = repo.head_id() {
        others.push(head.detach());
    }
    let references = repo.references().map_err(git_err(OP, &full_name))?;
    if let Ok(branches) = references.local_branches() {
        for branch in branches.flatten() {
            if branch.name().as_bstr() == full_name.as_str() {
                continue;
            }
            if let Ok(id) = branch.into_fully_peeled_id() {
                others.push(id.detach());
            }
        }
    }
    for other in others {
        if other == tip {
            return Ok(true);
        }
        if let Ok(base) = repo.merge_base(tip, other)
            && base.detach() == tip
        {
            return Ok(true);
        }
    }
    Ok(false)
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
