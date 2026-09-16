//! Decides what a user must do before rc-git runs a git action that can lose work.
//!
//! The rules live here, in one place, as data a test can enumerate. A front end asks
//! [`Policy::evaluate`] and obeys the [`Decision`]; it never decides on its own. Nothing in
//! rc-git mutates a repository today. When something will, its entry point takes a
//! [`Confirmation`] for every [`Risk::Destructive`] action, and the only way to get one is
//! [`Challenge::verify`].

pub mod actions;

/// A git operation a user might ask for, described by what it would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Download objects and update remote-tracking refs. Never loses local work.
    Fetch,
    /// Create a local branch at an existing commit, never overwriting one. Loses nothing.
    CreateLocalBranch {
        /// Branch name without `refs/heads/`.
        name: String,
    },
    /// Create a tag at an existing commit, never overwriting one. Loses nothing.
    CreateTag {
        /// Tag name without `refs/tags/`.
        name: String,
    },
    /// Move `HEAD` to another commit or branch. Undone by checking out again.
    Checkout {
        /// Branch, tag, or commit being checked out.
        target: String,
    },
    /// Move the checked-out branch forward to its upstream when the upstream strictly
    /// contains it. Refused in the write path when any affected file is dirty, so it
    /// overwrites nothing.
    PullFastForward {
        /// Branch being advanced, without `refs/heads/`.
        branch: String,
    },
    /// Upload commits. Without `force`, the server rejects a push that would drop history.
    Push {
        /// Branch being pushed, without `refs/heads/`.
        branch: String,
        /// Whether the push overwrites the remote branch regardless of its history.
        force: bool,
    },
    /// Delete a local branch. The reflog keeps its commits for a while.
    DeleteLocalBranch {
        /// Branch name without `refs/heads/`.
        name: String,
        /// Whether every commit is reachable from another branch.
        merged: bool,
    },
    /// Delete a branch on a remote. The remote keeps no reflog the user can reach.
    DeleteRemoteBranch {
        /// Branch name on the remote.
        name: String,
    },
    /// Delete a tag.
    DeleteTag {
        /// Tag name without `refs/tags/`.
        name: String,
    },
    /// `git reset --hard`: moves the branch back and discards every uncommitted change.
    ResetHard {
        /// Branch being reset.
        branch: String,
        /// How many commits the branch loses.
        commits_dropped: usize,
    },
    /// Rebase, amend, or squash.
    RewriteHistory {
        /// Branch being rewritten.
        branch: String,
        /// Whether any rewritten commit was already pushed somewhere.
        published: bool,
    },
    /// Throw away uncommitted changes. Nothing in git remembers them afterwards.
    DiscardWorkingChanges {
        /// Number of files affected.
        files: usize,
    },
    /// `git gc --prune` or `reflog expire`: deletes unreachable objects, which is the undo
    /// for every action above.
    Prune,
    /// Commit selected working-tree changes to the checked-out branch. Adds objects and
    /// advances the branch; loses nothing.
    Commit {
        /// Number of files in the commit.
        files: usize,
    },
    /// Record selected changes as a stash commit, then restore those files to `HEAD`
    /// content. The stash keeps everything it removed from disk.
    StashPush {
        /// Number of files being stashed.
        files: usize,
    },
    /// Write a stash's changes back to the working tree, keeping the stash entry. Refused
    /// in the write path when any affected file is dirty, so it overwrites nothing.
    StashApply,
    /// Apply a stash and drop it once every file is restored. The reflog line goes away,
    /// but the restored changes are the content it held.
    StashPop,
    /// Delete a stash entry without applying it. Nothing else remembers those changes.
    StashDrop {
        /// The entry being dropped, e.g. `stash@{1}`.
        name: String,
    },
}

/// How much an action can lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Risk {
    /// Cannot lose work.
    Safe,
    /// Loses something git still remembers, so it can be brought back.
    Recoverable,
    /// Loses something nothing remembers.
    Destructive,
}

impl Action {
    /// The worst case for this action, before any policy is applied.
    #[must_use]
    pub fn risk(&self) -> Risk {
        match self {
            Action::Fetch
            | Action::CreateLocalBranch { .. }
            | Action::CreateTag { .. }
            | Action::Checkout { .. }
            | Action::PullFastForward { .. }
            | Action::Push { force: false, .. }
            | Action::DiscardWorkingChanges { files: 0 }
            | Action::Commit { .. }
            | Action::StashApply => Risk::Safe,
            Action::DeleteLocalBranch { merged: true, .. }
            | Action::RewriteHistory {
                published: false, ..
            }
            | Action::StashPush { .. }
            | Action::StashPop => Risk::Recoverable,
            Action::Push { force: true, .. }
            | Action::DeleteLocalBranch { merged: false, .. }
            | Action::DeleteRemoteBranch { .. }
            | Action::DeleteTag { .. }
            | Action::ResetHard { .. }
            | Action::RewriteHistory {
                published: true, ..
            }
            | Action::DiscardWorkingChanges { .. }
            | Action::Prune
            | Action::StashDrop { .. } => Risk::Destructive,
        }
    }
}

/// What the front end must collect before an action may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// Why the user is being asked.
    pub risk: Risk,
    /// One sentence saying what will happen.
    pub summary: String,
    /// Text the user must type exactly. `None` means a plain confirm button is enough.
    pub typed_phrase: Option<String>,
    /// Whether to record a backup ref under `refs/rc-git/backup/` before running, so the
    /// previous state can be restored.
    pub backup_ref: bool,
}

impl Challenge {
    /// Checks the user's typed input. Surrounding whitespace is ignored; nothing else is.
    #[must_use]
    pub fn verify(&self, input: &str) -> Option<Confirmation> {
        match &self.typed_phrase {
            None => Some(Confirmation(())),
            Some(phrase) if input.trim() == phrase => Some(Confirmation(())),
            Some(_) => None,
        }
    }
}

/// Proof that a [`Challenge`] was answered. It cannot be built outside this crate, so a
/// function that takes one cannot be called without going through [`Challenge::verify`].
#[derive(Debug, PartialEq, Eq)]
pub struct Confirmation(());

/// The policy's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run it.
    Allow,
    /// Run it after the user answers the challenge.
    Confirm(Challenge),
    /// Never run it from rc-git; the message says why.
    Deny {
        /// Shown to the user.
        reason: String,
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

    /// Whether `branch` matches a protected pattern.
    #[must_use]
    pub fn is_protected(&self, branch: &str) -> bool {
        let branch = strip_heads(branch);
        for pattern in &self.protected {
            if let Some(prefix) = pattern.strip_suffix("/*") {
                if branch
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
                {
                    return true;
                }
            } else if pattern == branch {
                return true;
            }
        }
        false
    }

    /// Decides what `action` needs. See `docs/safety.md` for the table this encodes.
    #[must_use]
    pub fn evaluate(&self, action: &Action) -> Decision {
        let risk = action.risk();
        match action {
            Action::Fetch
            | Action::CreateLocalBranch { .. }
            | Action::CreateTag { .. }
            | Action::Checkout { .. }
            | Action::PullFastForward { .. }
            | Action::Push { force: false, .. }
            | Action::DiscardWorkingChanges { files: 0 }
            | Action::Commit { .. }
            | Action::StashApply => Decision::Allow,
            Action::Push { branch, force: true } => {
                if self.is_protected(branch) {
                    return deny(&format!("force-pushing to protected branch '{branch}'"));
                }
                confirm(risk, format!("Overwrite '{branch}' on the remote with your local history."), Some(branch), false)
            }
            Action::DeleteLocalBranch { name, merged } => {
                if self.is_protected(name) {
                    return deny(&format!("deleting protected branch '{name}'"));
                }
                if *merged {
                    return confirm(risk, format!("Delete branch '{name}'. Its commits stay reachable elsewhere."), None, false);
                }
                confirm(risk, format!("Delete branch '{name}' and its unmerged commits."), Some(name), true)
            }
            Action::DeleteRemoteBranch { name } => {
                if self.is_protected(name) {
                    return deny(&format!("deleting protected remote branch '{name}'"));
                }
                confirm(risk, format!("Delete '{name}' on the remote for everyone."), Some(name), true)
            }
            Action::DeleteTag { name } => {
                confirm(risk, format!("Delete tag '{name}'."), Some(name), true)
            }
            Action::ResetHard { branch, commits_dropped } => confirm(
                risk,
                format!("Drop {commits_dropped} commit(s) from '{branch}' and every uncommitted change."),
                Some(branch),
                true,
            ),
            Action::RewriteHistory { branch, published } => {
                if *published && self.is_protected(branch) {
                    return deny(&format!("rewriting published history on protected branch '{branch}'"));
                }
                if *published {
                    return confirm(risk, format!("Rewrite commits on '{branch}' that were already pushed."), Some(branch), true);
                }
                confirm(risk, format!("Rewrite local commits on '{branch}'. The reflog keeps the old ones."), None, true)
            }
            Action::DiscardWorkingChanges { files } => confirm(
                risk,
                format!("Discard uncommitted changes in {files} file(s). This cannot be undone."),
                Some("discard"),
                false,
            ),
            Action::Prune => confirm(
                risk,
                "Delete unreachable objects. Nothing deleted before this can be recovered afterwards.".to_owned(),
                Some("prune"),
                false,
            ),
            Action::StashPush { files } => confirm(
                risk,
                format!("Stash the changes in {files} file(s) and restore them to HEAD content."),
                None,
                false,
            ),
            Action::StashPop => confirm(
                risk,
                "Apply the stash to the working tree and drop it once every file is restored."
                    .to_owned(),
                None,
                true,
            ),
            Action::StashDrop { name } => confirm(
                risk,
                format!("Delete {name} without applying it. This cannot be undone."),
                Some("drop"),
                true,
            ),
        }
    }
}

fn strip_heads(name: &str) -> &str {
    name.strip_prefix("refs/heads/").unwrap_or(name)
}

fn confirm(risk: Risk, summary: String, typed_phrase: Option<&str>, backup_ref: bool) -> Decision {
    Decision::Confirm(Challenge {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(decision: Decision) -> Challenge {
        match decision {
            Decision::Confirm(challenge) => challenge,
            Decision::Allow | Decision::Deny { .. } => panic!("expected Confirm, got {decision:?}"),
        }
    }

    #[test]
    fn protected_patterns_match_exact_names_and_namespaces() {
        let policy = Policy::default();
        assert!(policy.is_protected("main"));
        assert!(policy.is_protected("refs/heads/main"));
        assert!(policy.is_protected("master"));
        assert!(policy.is_protected("release/1.2"));
        assert!(policy.is_protected("release/1.2/hotfix"));
        assert!(!policy.is_protected("release"));
        assert!(!policy.is_protected("releases/1.2"));
        assert!(!policy.is_protected("maintenance"));
        assert!(!policy.is_protected("feature/main"));

        let custom = Policy::new(["refs/heads/trunk", "hotfix/*"]);
        assert!(custom.is_protected("trunk"));
        assert!(custom.is_protected("hotfix/x"));
        assert!(!custom.is_protected("main"));
    }

    #[test]
    fn safe_actions_are_allowed_outright() {
        let policy = Policy::default();
        assert_eq!(policy.evaluate(&Action::Fetch), Decision::Allow);
        assert_eq!(
            policy.evaluate(&Action::CreateLocalBranch {
                name: "main".into()
            }),
            Decision::Allow,
            "creation never overwrites, so even a protected name is safe"
        );
        assert_eq!(
            policy.evaluate(&Action::CreateTag { name: "v9".into() }),
            Decision::Allow
        );
        assert_eq!(
            policy.evaluate(&Action::Checkout {
                target: "main".into()
            }),
            Decision::Allow
        );
        assert_eq!(
            policy.evaluate(&Action::PullFastForward {
                branch: "main".into()
            }),
            Decision::Allow,
            "a fast-forward loses nothing; the write path refuses dirty files itself"
        );
        assert_eq!(
            policy.evaluate(&Action::Push {
                branch: "main".into(),
                force: false
            }),
            Decision::Allow
        );
        assert_eq!(
            policy.evaluate(&Action::DiscardWorkingChanges { files: 0 }),
            Decision::Allow
        );
        assert_eq!(
            policy.evaluate(&Action::Commit { files: 3 }),
            Decision::Allow,
            "a commit adds objects and advances the checked-out branch; it loses nothing"
        );
        assert_eq!(
            policy.evaluate(&Action::StashApply),
            Decision::Allow,
            "the write path refuses dirty files itself, so apply overwrites nothing"
        );
    }

    #[test]
    fn stash_actions_scale_with_what_is_lost() {
        let policy = Policy::default();
        let push = challenge(policy.evaluate(&Action::StashPush { files: 2 }));
        assert_eq!(
            (push.risk, push.typed_phrase.as_deref(), push.backup_ref),
            (Risk::Recoverable, None, false),
            "the stash commit is the record; there is nothing else to back up"
        );
        assert!(push.verify("").is_some(), "no phrase means a plain confirm");

        let pop = challenge(policy.evaluate(&Action::StashPop));
        assert_eq!(
            (pop.risk, pop.typed_phrase.as_deref(), pop.backup_ref),
            (Risk::Recoverable, None, true)
        );

        let drop = challenge(policy.evaluate(&Action::StashDrop {
            name: "stash@{1}".into(),
        }));
        assert_eq!(
            (drop.risk, drop.typed_phrase.as_deref(), drop.backup_ref),
            (Risk::Destructive, Some("drop"), true)
        );
        assert!(drop.summary.contains("stash@{1}"), "{}", drop.summary);
        assert!(drop.verify("drop").is_some());
        assert!(drop.verify("Drop").is_none());
    }

    #[test]
    fn force_push_needs_the_branch_name_or_is_denied() {
        let policy = Policy::default();
        let c = challenge(policy.evaluate(&Action::Push {
            branch: "topic".into(),
            force: true,
        }));
        assert_eq!(c.risk, Risk::Destructive);
        assert_eq!(c.typed_phrase.as_deref(), Some("topic"));
        assert!(c.verify("topic").is_some());
        assert!(c.verify("  topic\n").is_some());
        assert!(c.verify("Topic").is_none());
        assert!(c.verify("").is_none());

        let denied = policy.evaluate(&Action::Push {
            branch: "main".into(),
            force: true,
        });
        assert!(matches!(denied, Decision::Deny { ref reason } if reason.contains("'main'")));
    }

    #[test]
    fn branch_deletion_scales_with_what_is_lost() {
        let policy = Policy::default();
        let merged = challenge(policy.evaluate(&Action::DeleteLocalBranch {
            name: "topic".into(),
            merged: true,
        }));
        assert_eq!(
            (
                merged.risk,
                merged.typed_phrase.as_deref(),
                merged.backup_ref
            ),
            (Risk::Recoverable, None, false)
        );
        assert!(
            merged.verify("anything").is_some(),
            "no phrase means a plain confirm"
        );

        let unmerged = challenge(policy.evaluate(&Action::DeleteLocalBranch {
            name: "topic".into(),
            merged: false,
        }));
        assert_eq!(unmerged.risk, Risk::Destructive);
        assert_eq!(unmerged.typed_phrase.as_deref(), Some("topic"));
        assert!(unmerged.backup_ref);

        assert!(matches!(
            policy.evaluate(&Action::DeleteLocalBranch {
                name: "main".into(),
                merged: true
            }),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            policy.evaluate(&Action::DeleteRemoteBranch {
                name: "release/2.0".into()
            }),
            Decision::Deny { .. }
        ));
        let remote = challenge(policy.evaluate(&Action::DeleteRemoteBranch {
            name: "topic".into(),
        }));
        assert_eq!(remote.typed_phrase.as_deref(), Some("topic"));
        let tag = challenge(policy.evaluate(&Action::DeleteTag {
            name: "v1.0".into(),
        }));
        assert_eq!(
            (tag.risk, tag.typed_phrase.as_deref()),
            (Risk::Destructive, Some("v1.0"))
        );
    }

    #[test]
    fn history_rewrites_depend_on_publication() {
        let policy = Policy::default();
        let local = challenge(policy.evaluate(&Action::RewriteHistory {
            branch: "topic".into(),
            published: false,
        }));
        assert_eq!(
            (local.risk, local.typed_phrase.as_deref(), local.backup_ref),
            (Risk::Recoverable, None, true)
        );

        let published = challenge(policy.evaluate(&Action::RewriteHistory {
            branch: "topic".into(),
            published: true,
        }));
        assert_eq!(published.risk, Risk::Destructive);
        assert_eq!(published.typed_phrase.as_deref(), Some("topic"));

        assert!(matches!(
            policy.evaluate(&Action::RewriteHistory {
                branch: "main".into(),
                published: true
            }),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            policy.evaluate(&Action::RewriteHistory {
                branch: "main".into(),
                published: false
            }),
            Decision::Confirm(_)
        ));
    }

    #[test]
    fn reset_discard_and_prune_always_challenge() {
        let policy = Policy::default();
        let reset = challenge(policy.evaluate(&Action::ResetHard {
            branch: "main".into(),
            commits_dropped: 2,
        }));
        assert_eq!(reset.typed_phrase.as_deref(), Some("main"));
        assert!(reset.backup_ref);
        assert!(reset.summary.contains("2 commit(s)"));

        let discard = challenge(policy.evaluate(&Action::DiscardWorkingChanges { files: 3 }));
        assert_eq!(discard.typed_phrase.as_deref(), Some("discard"));
        assert!(
            !discard.backup_ref,
            "uncommitted changes have nothing to back up into a ref"
        );

        let prune = challenge(policy.evaluate(&Action::Prune));
        assert_eq!(
            (prune.risk, prune.typed_phrase.as_deref()),
            (Risk::Destructive, Some("prune"))
        );
    }

    #[test]
    fn risk_orders_from_safe_to_destructive() {
        assert!(Risk::Safe < Risk::Recoverable);
        assert!(Risk::Recoverable < Risk::Destructive);
        assert_eq!(Action::Fetch.risk(), Risk::Safe);
        assert_eq!(Action::Prune.risk(), Risk::Destructive);
    }

    #[test]
    fn denial_tells_the_user_where_to_go() {
        let Decision::Deny { reason } = Policy::default().evaluate(&Action::Push {
            branch: "master".into(),
            force: true,
        }) else {
            panic!("expected Deny");
        };
        assert_eq!(
            reason,
            "rc-git never allows force-pushing to protected branch 'master'. Use git directly if you are sure."
        );
    }
}
