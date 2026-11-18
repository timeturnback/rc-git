//! The policy, its challenges, and the proof that one was answered. Everything that mints a
//! [`Challenge`] or a [`Confirmation`] lives in this module, and their fields are private to
//! it, so no other code in the crate — `actions` included — can build either one by hand.

use crate::{Action, Decision, Risk};

/// What the front end must collect before an action may run. Only [`Policy::evaluate`] builds
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// The action it was issued for, carried into the proof it verifies to.
    action: Action,
    risk: Risk,
    summary: String,
    typed_phrase: Option<String>,
    backup_ref: bool,
}

impl Challenge {
    /// Why the user is being asked.
    #[must_use]
    pub fn risk(&self) -> Risk {
        self.risk
    }

    /// One sentence saying what will happen.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Text the user must type exactly. `None` means a plain confirm button is enough.
    #[must_use]
    pub fn typed_phrase(&self) -> Option<&str> {
        self.typed_phrase.as_deref()
    }

    /// Whether to record a backup ref under `refs/rc-git/backup/` before running, so the
    /// previous state can be restored.
    #[must_use]
    pub fn backup_ref(&self) -> bool {
        self.backup_ref
    }

    /// Checks the user's typed input. Surrounding whitespace is ignored; nothing else is.
    #[must_use]
    pub fn verify(&self, input: &str) -> Option<Confirmation> {
        match &self.typed_phrase {
            Some(phrase) if input.trim() != phrase => None,
            Some(_) | None => Some(Confirmation {
                action: self.action.clone(),
                backup_ref: self.backup_ref,
            }),
        }
    }
}

/// Proof that the policy allowed an action, or that its [`Challenge`] was answered. Only
/// [`Challenge::verify`] and the crate's own authorization build one, and every write in
/// `actions` needs one, so nothing writes without a policy decision. It carries the action it
/// was issued for, so a write that reads its arguments from the proof (a GitHub mutation) can
/// only perform that action.
#[derive(Debug, PartialEq, Eq)]
pub struct Confirmation {
    action: Action,
    backup_ref: bool,
}

impl Confirmation {
    /// Whether the answered challenge asked for a backup ref before the action runs.
    #[must_use]
    pub fn backup_ref(&self) -> bool {
        self.backup_ref
    }

    /// The action this proof was issued for.
    pub(crate) fn action(&self) -> &Action {
        &self.action
    }
}

/// Why [`Policy::authorize`] gave no [`Confirmation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The policy denies the action outright; the reason is ready to show.
    Denied(String),
    /// The action needs a confirmation and the input did not answer it.
    NotConfirmed {
        /// The text the challenge asks the user to type.
        phrase: String,
    },
}

/// Which branches are protected and what each action needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    protected: Vec<String>,
}

impl Default for Policy {
    /// Protects `main`, `master`, and `release/*`.
    fn default() -> Self {
        Self::new(["main", "master", "release/*"])
    }
}

impl Policy {
    /// A policy protecting the given branch patterns: exact names, or `prefix/*` for a
    /// namespace. `refs/heads/` is stripped from both patterns and queries.
    pub fn new(patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut protected = Vec::new();
        for pattern in patterns {
            protected.push(strip_heads(&pattern.into()).to_owned());
        }
        Self { protected }
    }

    /// Whether `branch` matches a protected pattern, ignoring ASCII case. A case-insensitive
    /// volume stores `MAIN` and `main` as one file, and the policy has no repository to ask,
    /// so it fails closed everywhere: in a repository holding both, `Main` is protected too.
    #[must_use]
    pub fn is_protected(&self, branch: &str) -> bool {
        let branch = strip_heads(branch).as_bytes();
        for pattern in &self.protected {
            if let Some(prefix) = pattern.strip_suffix("/*") {
                let prefix = prefix.as_bytes();
                if branch.len() > prefix.len()
                    && branch[..prefix.len()].eq_ignore_ascii_case(prefix)
                    && branch[prefix.len()] == b'/'
                {
                    return true;
                }
            } else if pattern.as_bytes().eq_ignore_ascii_case(branch) {
                return true;
            }
        }
        false
    }

    /// Decides what `action` needs. See `docs/safety.md` for the table this encodes.
    #[must_use]
    pub fn evaluate(&self, action: &Action) -> Decision {
        let risk = (action, action.risk());
        match action {
            Action::Fetch
            | Action::FetchPullRequest { .. }
            | Action::CreateLocalBranch { .. }
            | Action::CreateTag { .. }
            | Action::Checkout { .. }
            | Action::PullFastForward { .. }
            | Action::Push { force: false, .. }
            | Action::DiscardWorkingChanges { files: 0 }
            | Action::Commit { .. }
            | Action::StashApply => Decision::Allow,
            Action::Push {
                branch,
                force: true,
            } => self.force_push(risk, branch),
            Action::DeleteLocalBranch { name, merged } => {
                self.delete_local_branch(risk, name, *merged)
            }
            Action::DeleteRemoteBranch { name } => self.delete_remote(risk, name),
            Action::DeleteTag { name } => {
                confirm(risk, format!("Delete tag '{name}'."), Some(name), true)
            }
            Action::ResetHard {
                branch,
                commits_dropped,
            } => confirm(
                risk,
                format!(
                    "Drop {commits_dropped} commit(s) from '{branch}' \
                     and every uncommitted change."
                ),
                Some(branch),
                true,
            ),
            Action::RewriteHistory { branch, published } => self.rewrite(risk, branch, *published),
            Action::DiscardWorkingChanges { files } => confirm(
                risk,
                format!("Discard uncommitted changes in {files} file(s). This cannot be undone."),
                None,
                false,
            ),
            Action::Prune => confirm(
                risk,
                "Delete unreachable objects. \
                 Nothing deleted before this can be recovered afterwards."
                    .to_owned(),
                Some("prune"),
                false,
            ),
            Action::StashPush { files } => confirm(
                risk,
                format!("Stash the changes in {files} file(s) and restore them to HEAD content."),
                None,
                false,
            ),
            Action::StashPop { name } => confirm(
                risk,
                format!(
                    "Apply {name} to the working tree and drop it once every file is restored."
                ),
                None,
                true,
            ),
            Action::StashDrop { name } => confirm(
                risk,
                format!("Delete {name} without applying it. This cannot be undone."),
                Some("drop"),
                true,
            ),
            Action::MergePullRequest(merge) => confirm(risk, merge.summary(), None, false),
        }
    }

    /// Force-pushing `branch`: denied when protected, otherwise confirmed by typing its name.
    fn force_push(&self, risk: Assessed<'_>, branch: &str) -> Decision {
        if self.is_protected(branch) {
            return deny(&format!("force-pushing to protected branch '{branch}'"));
        }
        let summary = format!("Overwrite '{branch}' on the remote with your local history.");
        confirm(risk, summary, Some(branch), false)
    }

    /// Deleting local branch `name`: denied when protected; a merged branch needs a plain
    /// confirm, an unmerged one its typed name and a backup ref.
    fn delete_local_branch(&self, risk: Assessed<'_>, name: &str, merged: bool) -> Decision {
        if self.is_protected(name) {
            return deny(&format!("deleting protected branch '{name}'"));
        }
        if merged {
            let summary = format!("Delete branch '{name}'. Its commits stay reachable elsewhere.");
            return confirm(risk, summary, None, false);
        }
        let summary = format!("Delete branch '{name}' and its unmerged commits.");
        confirm(risk, summary, Some(name), true)
    }

    /// Deleting remote branch `name`: denied when protected, otherwise confirmed by typing its
    /// name, with a backup ref.
    fn delete_remote(&self, risk: Assessed<'_>, name: &str) -> Decision {
        if self.is_protected(name) {
            return deny(&format!("deleting protected remote branch '{name}'"));
        }
        let summary = format!("Delete '{name}' on the remote for everyone.");
        confirm(risk, summary, Some(name), true)
    }

    /// Rewriting commits on `branch`: published history is denied on a protected branch and
    /// needs the typed name elsewhere; local-only history needs a plain confirm.
    fn rewrite(&self, risk: Assessed<'_>, branch: &str, published: bool) -> Decision {
        if !published {
            let summary =
                format!("Rewrite local commits on '{branch}'. The reflog keeps the old ones.");
            return confirm(risk, summary, None, true);
        }
        if self.is_protected(branch) {
            return deny(&format!(
                "rewriting published history on protected branch '{branch}'"
            ));
        }
        let summary = format!("Rewrite commits on '{branch}' that were already pushed.");
        confirm(risk, summary, Some(branch), true)
    }

    /// Evaluates `action` and turns the user's `input` into proof: an [`Decision::Allow`]
    /// needs no input, a [`Decision::Confirm`] needs input its challenge verifies (`None` reads
    /// as empty), and a [`Decision::Deny`] is final. The decision is made here, from the
    /// action itself, so a caller cannot hand in one it built.
    pub(crate) fn authorize(
        &self,
        action: &Action,
        input: Option<&str>,
    ) -> Result<Confirmation, Refusal> {
        match self.evaluate(action) {
            Decision::Allow => Ok(Confirmation {
                action: action.clone(),
                backup_ref: false,
            }),
            Decision::Confirm(challenge) => {
                challenge
                    .verify(input.unwrap_or(""))
                    .ok_or_else(|| Refusal::NotConfirmed {
                        // A challenge without a phrase verifies any input, so it never gets here.
                        phrase: challenge.typed_phrase().unwrap_or_default().to_owned(),
                    })
            }
            Decision::Deny { reason } => Err(Refusal::Denied(reason)),
        }
    }
}

fn strip_heads(name: &str) -> &str {
    name.strip_prefix("refs/heads/").unwrap_or(name)
}

/// The action being decided and its risk, which every challenge carries.
type Assessed<'a> = (&'a Action, Risk);

fn confirm(
    (action, risk): Assessed<'_>,
    summary: String,
    typed_phrase: Option<&str>,
    backup_ref: bool,
) -> Decision {
    Decision::Confirm(Challenge {
        action: action.clone(),
        risk,
        summary,
        typed_phrase: typed_phrase.map(str::to_owned),
        backup_ref,
    })
}

fn deny(what: &str) -> Decision {
    Decision::Deny {
        reason: format!("rc-git never allows {what}. Use git directly if you are sure."),
    }
}
