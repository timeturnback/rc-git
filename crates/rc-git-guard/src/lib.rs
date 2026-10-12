//! Decides what a user must do before rc-git runs a git action that can lose work, and runs
//! the actions that pass.
//!
//! The rules live here, in one place, as data a test can enumerate. A front end asks
//! [`Policy::evaluate`] and obeys the [`Decision`]; it never decides on its own. [`actions`] is
//! the one write path in rc-git: each entry point re-evaluates the policy and turns the
//! caller's typed input into a [`Confirmation`] through [`Challenge::verify`] before anything
//! is written, and every write primitive needs that proof. A [`Decision::Deny`] is final.

pub mod actions;
mod policy;

pub(crate) use policy::Refusal;
pub use policy::{Challenge, Confirmation, Policy};

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
    StashPop {
        /// The entry being popped and its message, e.g. `stash@{1} "On main: wip"`.
        name: String,
    },
    /// Delete a stash entry without applying it. Nothing else remembers those changes.
    StashDrop {
        /// The entry being dropped and its message, e.g. `stash@{1} "On main: wip"`.
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
            | Action::StashPop { .. } => Risk::Recoverable,
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
        assert!(policy.is_protected("MAIN"), "case variants fail closed");
        assert!(policy.is_protected("Main"));
        assert!(policy.is_protected("refs/heads/Master"));
        assert!(policy.is_protected("Release/1.2"));
        assert!(policy.is_protected("RELEASE/x"));
        assert!(!policy.is_protected("RELEASE"));
        assert!(!policy.is_protected("Releases/1.2"));

        let custom = Policy::new(["refs/heads/trunk", "hotfix/*"]);
        assert!(custom.is_protected("trunk"));
        assert!(custom.is_protected("Trunk"));
        assert!(custom.is_protected("HotFix/x"));
        assert!(custom.is_protected("hotfix/x"));
        assert!(!custom.is_protected("main"));
    }

    #[test]
    fn risk_orders_from_safe_to_destructive() {
        assert!(Risk::Safe < Risk::Recoverable);
        assert!(Risk::Recoverable < Risk::Destructive);
        assert_eq!(Action::Fetch.risk(), Risk::Safe);
        assert_eq!(Action::Prune.risk(), Risk::Destructive);
    }

    /// What `docs/safety.md`'s table says one action gets.
    #[derive(Debug, PartialEq, Eq)]
    enum Expected {
        Allow,
        Deny,
        Confirm {
            risk: Risk,
            phrase: Option<String>,
            backup: bool,
        },
    }

    impl Expected {
        fn confirm(risk: Risk, phrase: Option<&str>, backup: bool) -> Self {
            Expected::Confirm {
                risk,
                phrase: phrase.map(str::to_owned),
                backup,
            }
        }

        fn of(decision: Decision) -> Self {
            match decision {
                Decision::Allow => Expected::Allow,
                Decision::Deny { .. } => Expected::Deny,
                Decision::Confirm(c) => Expected::Confirm {
                    risk: c.risk(),
                    phrase: c.typed_phrase().map(str::to_owned),
                    backup: c.backup_ref(),
                },
            }
        }
    }

    /// The safety table, one arm per row. The match has no wildcard, so a new `Action`
    /// variant does not compile until it has a row here.
    fn safety_table_row(action: &Action, protected: bool) -> Expected {
        let unless_protected = |row: Expected| if protected { Expected::Deny } else { row };
        match action {
            Action::Fetch
            | Action::CreateLocalBranch { .. }
            | Action::CreateTag { .. }
            | Action::Checkout { .. }
            | Action::PullFastForward { .. }
            | Action::Push { force: false, .. }
            | Action::DiscardWorkingChanges { files: 0 }
            | Action::Commit { .. }
            | Action::StashApply => Expected::Allow,
            Action::Push {
                branch,
                force: true,
            } => unless_protected(Expected::confirm(Risk::Destructive, Some(branch), false)),
            Action::DeleteLocalBranch { merged: true, .. } => {
                unless_protected(Expected::confirm(Risk::Recoverable, None, false))
            }
            Action::DeleteLocalBranch {
                name,
                merged: false,
            }
            | Action::DeleteRemoteBranch { name } => {
                unless_protected(Expected::confirm(Risk::Destructive, Some(name), true))
            }
            Action::DeleteTag { name } => Expected::confirm(Risk::Destructive, Some(name), true),
            Action::ResetHard { branch, .. } => {
                Expected::confirm(Risk::Destructive, Some(branch), true)
            }
            Action::RewriteHistory {
                published: false, ..
            }
            | Action::StashPop { .. } => Expected::confirm(Risk::Recoverable, None, true),
            Action::RewriteHistory {
                branch,
                published: true,
            } => unless_protected(Expected::confirm(Risk::Destructive, Some(branch), true)),
            Action::DiscardWorkingChanges { .. } => {
                Expected::confirm(Risk::Destructive, None, false)
            }
            Action::Prune => Expected::confirm(Risk::Destructive, Some("prune"), false),
            Action::StashPush { .. } => Expected::confirm(Risk::Recoverable, None, false),
            Action::StashDrop { .. } => Expected::confirm(Risk::Destructive, Some("drop"), true),
        }
    }

    fn one_of_each_action(name: &str) -> Vec<Action> {
        let name = || name.to_owned();
        let stash = || "stash@{0} \"On topic: wip\"".to_owned();
        vec![
            Action::Fetch,
            Action::CreateLocalBranch { name: name() },
            Action::CreateTag { name: name() },
            Action::Checkout { target: name() },
            Action::PullFastForward { branch: name() },
            Action::Push {
                branch: name(),
                force: false,
            },
            Action::Push {
                branch: name(),
                force: true,
            },
            Action::DeleteLocalBranch {
                name: name(),
                merged: true,
            },
            Action::DeleteLocalBranch {
                name: name(),
                merged: false,
            },
            Action::DeleteRemoteBranch { name: name() },
            Action::DeleteTag { name: name() },
            Action::ResetHard {
                branch: name(),
                commits_dropped: 1,
            },
            Action::RewriteHistory {
                branch: name(),
                published: false,
            },
            Action::RewriteHistory {
                branch: name(),
                published: true,
            },
            Action::DiscardWorkingChanges { files: 0 },
            Action::DiscardWorkingChanges { files: 1 },
            Action::Prune,
            Action::Commit { files: 1 },
            Action::StashPush { files: 1 },
            Action::StashApply,
            Action::StashPop { name: stash() },
            Action::StashDrop { name: stash() },
        ]
    }

    #[test]
    fn every_action_gets_its_safety_table_row() {
        let policy = Policy::default();
        let names = [
            ("topic", false),
            ("feature/main", false),
            ("main", true),
            ("MAIN", true),
            ("release/2.0", true),
        ];
        for (name, protected) in names {
            for action in one_of_each_action(name) {
                assert_eq!(
                    Expected::of(policy.evaluate(&action)),
                    safety_table_row(&action, protected),
                    "{action:?}"
                );
            }
        }
    }

    #[test]
    fn allow_is_exactly_the_safe_risk_and_a_challenge_carries_the_action_risk() {
        let policy = Policy::default();
        for action in one_of_each_action("topic") {
            let risk = action.risk();
            match policy.evaluate(&action) {
                Decision::Allow => assert_eq!(risk, Risk::Safe, "{action:?} is allowed"),
                Decision::Confirm(challenge) => {
                    assert_ne!(risk, Risk::Safe, "{action:?} is safe but challenged");
                    assert_eq!(challenge.risk(), risk, "{action:?}");
                }
                Decision::Deny { reason } => {
                    panic!("{action:?} on an unprotected name was denied: {reason}")
                }
            }
        }
    }

    #[test]
    fn a_typed_phrase_matches_exactly_after_trimming() {
        let policy = Policy::default();
        let c = challenge(policy.evaluate(&Action::Push {
            branch: "topic".into(),
            force: true,
        }));
        assert!(c.verify("topic").is_some());
        assert!(c.verify("  topic\n").is_some());
        assert!(c.verify("Topic").is_none());
        assert!(c.verify("").is_none());

        let drop = challenge(policy.evaluate(&Action::StashDrop {
            name: "stash@{1} \"On main: wip\"".into(),
        }));
        assert!(drop.verify("drop").is_some());
        assert!(drop.verify("Drop").is_none());

        let merged = challenge(policy.evaluate(&Action::DeleteLocalBranch {
            name: "topic".into(),
            merged: true,
        }));
        assert!(
            merged.verify("anything").is_some(),
            "no phrase means a plain confirm"
        );
    }

    #[test]
    fn a_challenge_summary_names_what_is_lost() {
        let policy = Policy::default();
        let pop = challenge(policy.evaluate(&Action::StashPop {
            name: "stash@{0} \"On main: wip\"".into(),
        }));
        assert!(
            pop.summary().contains("stash@{0} \"On main: wip\""),
            "{}",
            pop.summary()
        );
        let drop = challenge(policy.evaluate(&Action::StashDrop {
            name: "stash@{1} \"On main: wip\"".into(),
        }));
        assert!(
            drop.summary().contains("stash@{1} \"On main: wip\""),
            "{}",
            drop.summary()
        );
        let reset = challenge(policy.evaluate(&Action::ResetHard {
            branch: "main".into(),
            commits_dropped: 2,
        }));
        assert!(
            reset.summary().contains("2 commit(s)"),
            "{}",
            reset.summary()
        );
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

    #[test]
    fn authorize_turns_each_decision_into_proof_or_a_refusal() {
        let policy = Policy::default();
        let allowed = policy.authorize(&Action::Fetch, None).unwrap();
        assert!(
            !allowed.backup_ref(),
            "an allowed action has nothing to back up"
        );

        let delete = Action::DeleteTag {
            name: "v1".to_owned(),
        };
        assert_eq!(
            policy.authorize(&delete, None),
            Err(Refusal::NotConfirmed {
                phrase: "v1".to_owned()
            }),
            "a typed phrase is not answered by no input"
        );
        assert_eq!(
            policy.authorize(&delete, Some("v2")),
            Err(Refusal::NotConfirmed {
                phrase: "v1".to_owned()
            })
        );
        let confirmed = policy.authorize(&delete, Some(" v1\n")).unwrap();
        assert!(
            confirmed.backup_ref(),
            "the proof carries the challenge's backup request"
        );

        let force = Action::Push {
            branch: "main".to_owned(),
            force: true,
        };
        let Err(Refusal::Denied(reason)) = policy.authorize(&force, Some("main")) else {
            panic!("a denied action must stay denied whatever is typed");
        };
        assert!(reason.contains("Use git directly"), "{reason}");
    }

    #[test]
    fn a_plain_confirm_needs_no_input_and_keeps_its_backup_request() {
        let pop = Action::StashPop {
            name: "stash@{0} \"On main: wip\"".to_owned(),
        };
        let proof = Policy::default().authorize(&pop, None).unwrap();
        assert!(proof.backup_ref());
    }
}
