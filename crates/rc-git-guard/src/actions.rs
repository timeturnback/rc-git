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

/// Applies one ref edit as the synthetic `rc-git` committer, so reflog entries never depend
/// on user configuration the isolated open refuses to read.
fn apply_edit(
    repo: &gix::Repository,
    edit: RefEdit,
    op: &'static str,
    name: &str,
) -> ActionResult<()> {
    let time = gix::date::Time::now_local_or_utc()
        .format(gix::date::time::Format::Raw)
        .unwrap_or_else(|_| "0 +0000".to_owned());
    let committer = gix::actor::SignatureRef {
        name: "rc-git".into(),
        email: "rc-git@localhost".into(),
        time: &time,
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the guard crate is the one sanctioned write path; see docs/safety.md"
    )]
    repo.edit_references_as(Some(edit), Some(committer))
        .map_err(|source| ActionError::Git {
            op,
            name: name.to_owned(),
            source: Box::new(source),
        })?;
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
    /// The underlying git operation failed.
    #[error("{op} '{name}': {source}")]
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
    if let Ok(Some(head_name)) = repo.head_name()
        && head_name.as_bstr() == full_name.as_str()
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
    let decision = policy.evaluate(&Action::DeleteTag {
        name: name.to_owned(),
    });
    let confirmation = confirm(decision, OP, name, input)?;
    delete_ref(
        &repo,
        OP,
        &format!("refs/tags/{name}"),
        "tag",
        name,
        confirmation,
    )
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
    gix::ThreadSafeRepository::open_opts(repo_path, gix::open::Options::isolated())
        .map(|shared| shared.to_thread_local())
        .map_err(|source| ActionError::Git {
            op,
            name: name.to_owned(),
            source: Box::new(source),
        })
}

fn create_ref(
    repo_path: &Path,
    op: &'static str,
    full_name: &str,
    revspec: &str,
) -> ActionResult<()> {
    let repo = open(repo_path, op, full_name)?;
    let target = repo
        .rev_parse_single(revspec)
        .map_err(|source| ActionError::Git {
            op,
            name: revspec.to_owned(),
            source: Box::new(source),
        })?
        .detach();
    let edit = RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: format!("rc-git: {op}").into(),
            },
            expected: PreviousValue::MustNotExist,
            new: Target::Object(target),
        },
        name: full_name.try_into().map_err(|source| ActionError::Git {
            op,
            name: full_name.to_owned(),
            source: Box::new(source),
        })?,
        deref: false,
    };
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
        .map_err(|source| ActionError::Git {
            op,
            name: full_name.to_owned(),
            source: Box::new(source),
        })?;
    let target = reference.target().into_owned();
    let backup = if challenge.backup_ref {
        let target_id = match target.clone() {
            Target::Object(id) => id,
            Target::Symbolic(_) => reference
                .clone()
                .into_fully_peeled_id()
                .map_err(|source| ActionError::Git {
                    op,
                    name: full_name.to_owned(),
                    source: Box::new(source),
                })?
                .detach(),
        };
        Some(write_backup_ref(repo, backup_kind, short_name, target_id)?)
    } else {
        None
    };
    drop(reference);
    let edit = RefEdit {
        change: Change::Delete {
            expected: PreviousValue::MustExistAndMatch(target),
            log: RefLog::AndReference,
        },
        name: full_name.try_into().map_err(|source| ActionError::Git {
            op,
            name: full_name.to_owned(),
            source: Box::new(source),
        })?,
        deref: false,
    };
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
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: false,
                    message: "rc-git: backup before deletion".into(),
                },
                expected: PreviousValue::MustNotExist,
                new: Target::Object(target),
            },
            name: candidate
                .as_str()
                .try_into()
                .map_err(|source| ActionError::Git {
                    op: OP,
                    name: candidate.clone(),
                    source: Box::new(source),
                })?,
            deref: false,
        };
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
    let reference = repo
        .find_reference(&full_name)
        .map_err(|source| ActionError::Git {
            op: OP,
            name: full_name.clone(),
            source: Box::new(source),
        })?;
    let tip = reference
        .into_fully_peeled_id()
        .map_err(|source| ActionError::Git {
            op: OP,
            name: full_name.clone(),
            source: Box::new(source),
        })?
        .detach();
    let mut others: Vec<gix::ObjectId> = Vec::new();
    if let Ok(head) = repo.head_id() {
        others.push(head.detach());
    }
    let references = repo.references().map_err(|source| ActionError::Git {
        op: OP,
        name: full_name.clone(),
        source: Box::new(source),
    })?;
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
