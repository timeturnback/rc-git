//! The admin merge: merging a pull request that branch protection blocks, past that protection,
//! when GitHub says the signed-in account may (`viewerCanMergeAsAdmin`). GitHub has no separate
//! call for it: it is the plain merge's `mergePullRequest`, and GitHub decides from the account
//! whether it may go past the rules (the GitHub CLI's `gh pr merge --admin` sends the same
//! mutation). So everything rc-git adds is about never offering or sending one the user did not
//! knowingly confirm:
//!
//! - It exists only for a read GitHub reports as blocked by its requirements (a blocked or
//!   behind merge state) that is open, not a draft, free of conflicts, in a method the
//!   repository allows, and that the account may merge as an administrator
//!   ([`admin_merge_blocker`]). A state GitHub has not computed, a host that reports none, and a
//!   pull request the plain merge can take have none.
//! - Its challenge names what the read shows it skips ([`Bypassed`]) and says that rules the API
//!   does not report may be skipped too, never that the list is complete. It asks for the pull
//!   request's number, typed.
//! - The skipped list is part of the action, so a check that moved between the challenge and the
//!   confirm makes the answered decision differ from the policy's own, and nothing is sent.

use super::super::RepositoryTarget;
use super::super::pull_types::{
    CheckState, MergeMethod, MergeStateStatus, PullRequestDetail, PullRequestId, ReviewDecision,
};
use super::super::text::single_line;
use super::{MergeBlocker, PullRequestMerge, checks_phrase, label, merge_blocker, method_phrase};
use rc_git_core::Oid;

/// The longest check name a challenge quotes, in characters.
const CHECK_CHARS: usize = 80;

/// The most check names a challenge quotes per list; the rest are counted.
const NAMED_CHECKS: usize = 5;

/// What an admin merge skips, as the read it is built from reports it. Never the whole story:
/// branch protection and rulesets can require things GitHub's API does not report (the
/// challenge says so).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Bypassed {
    /// Required checks that failed, by name (single-line), in the read's order.
    pub failed: Vec<String>,
    /// Required checks not finished yet, by name (single-line), in the read's order.
    pub pending: Vec<String>,
    /// Checks that have not passed whose requirement the host did not report: they may be
    /// required.
    pub unreported: usize,
    /// Checks of the head the read did not hold.
    pub unread: usize,
    /// The review decision branch protection still waits on: a review required, or changes
    /// requested. `None` for any other decision.
    pub review: Option<ReviewDecision>,
    /// Whether the head branch is behind its base (GitHub's `BEHIND` merge state).
    pub behind: bool,
}

impl Bypassed {
    /// What `detail` shows an admin merge of it would skip.
    fn of(detail: &PullRequestDetail) -> Self {
        let review = match detail.summary.review {
            Some(review @ (ReviewDecision::ReviewRequired | ReviewDecision::ChangesRequested)) => {
                Some(review)
            }
            Some(ReviewDecision::Approved | ReviewDecision::Unknown) | None => None,
        };
        let checks = &detail.checks;
        let mut bypassed = Self {
            review,
            behind: detail.merge_state == Some(MergeStateStatus::Behind),
            unread: checks.total.saturating_sub(checks.checks.len()),
            ..Self::default()
        };
        for check in &checks.checks {
            let list = match check.progress.state() {
                CheckState::Failing => &mut bypassed.failed,
                CheckState::Pending => &mut bypassed.pending,
                CheckState::Passing | CheckState::Neutral => continue,
            };
            match check.required {
                Some(true) => list.push(check.name.clone()),
                Some(false) => {}
                None => bypassed.unreported += 1,
            }
        }
        bypassed
    }

    /// The challenge's list of what is known to be skipped, joined with `; `; `None` when the
    /// read names nothing.
    fn known(&self, base: &str) -> Option<String> {
        let mut items = Vec::new();
        if !self.failed.is_empty() {
            items.push(format!(
                "required checks that failed: {}",
                named(&self.failed)
            ));
        }
        if !self.pending.is_empty() {
            items.push(format!(
                "required checks not finished: {}",
                named(&self.pending)
            ));
        }
        match self.review {
            Some(ReviewDecision::ReviewRequired) => {
                items.push("a required review that has not approved it".to_owned());
            }
            Some(ReviewDecision::ChangesRequested) => {
                items.push("a review that asked for changes".to_owned());
            }
            Some(ReviewDecision::Approved | ReviewDecision::Unknown) | None => {}
        }
        if self.behind {
            items.push(format!("bringing its head branch up to date with {base}"));
        }
        (!items.is_empty()).then(|| items.join("; "))
    }

    /// What the read could not tell, as sentences; empty when it told everything it holds.
    fn uncertain(&self) -> String {
        let mut sentences = Vec::new();
        if self.unreported > 0 {
            sentences.push(format!(
                " {} more check(s) that have not passed may be required; GitHub did not say, \
                 so they may be skipped too.",
                self.unreported
            ));
        }
        if self.unread > 0 {
            sentences.push(format!(
                " The read did not hold {} of the head's checks; any of them may be required.",
                self.unread
            ));
        }
        sentences.concat()
    }
}

/// The names in curly quotes, `a, b, c, d, e and 3 more`: at most [`NAMED_CHECKS`] names, each
/// re-cut to [`CHECK_CHARS`] (they arrive single-line already).
fn named(names: &[String]) -> String {
    let mut quoted = Vec::new();
    for name in names.iter().take(NAMED_CHECKS) {
        quoted.push(format!(
            "\u{201c}{}\u{201d}",
            single_line(name, CHECK_CHARS)
        ));
    }
    let text = quoted.join(", ");
    match names.len().saturating_sub(NAMED_CHECKS) {
        0 => text,
        more => format!("{text} and {more} more"),
    }
}

/// An admin merge as the user confirms it: the merge it sends, every fact its challenge names,
/// and what it skips, all from the read made just before. Built only by
/// [`PullRequestAdminMerge::new`], from a read that branch protection blocks and that GitHub
/// says the account may merge past; the merge it wraps is never handed out, so it cannot become
/// a plain merge of a blocked pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestAdminMerge {
    merge: PullRequestMerge,
    bypassed: Bypassed,
}

impl PullRequestAdminMerge {
    /// The admin merge of `detail`, as read from `target`, with `method`.
    ///
    /// # Errors
    /// The [`MergeBlocker`] [`admin_merge_blocker`] finds.
    pub fn new(
        target: &RepositoryTarget,
        detail: &PullRequestDetail,
        method: MergeMethod,
    ) -> Result<Self, MergeBlocker> {
        if let Some(blocker) = admin_merge_blocker(detail, method) {
            return Err(blocker);
        }
        Ok(Self {
            merge: PullRequestMerge::read(target, detail, method)?,
            bypassed: Bypassed::of(detail),
        })
    }

    /// The repository.
    #[must_use]
    pub fn target(&self) -> &RepositoryTarget {
        self.merge.target()
    }

    /// The pull request's number.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.merge.number()
    }

    pub(in crate::actions::remote::github) fn id(&self) -> &PullRequestId {
        self.merge.id()
    }

    /// The head commit the merge expects.
    #[must_use]
    pub fn head_oid(&self) -> Oid {
        self.merge.head_oid()
    }

    /// How it merges.
    #[must_use]
    pub fn method(&self) -> MergeMethod {
        self.merge.method()
    }

    /// `owner/name#number`.
    #[must_use]
    pub fn label(&self) -> String {
        self.merge.label()
    }

    /// What it skips, as the read reports it.
    #[must_use]
    pub fn bypassed(&self) -> &Bypassed {
        &self.bypassed
    }

    /// What the challenge asks the user to type: the pull request's number in decimal digits,
    /// with no `#` and no leading zero.
    pub(crate) fn phrase(&self) -> String {
        self.merge.number.to_string()
    }

    /// The challenge's text: the pull request, its title, both branches, the head commit, the
    /// method and the checks; that it merges past branch protection; what the read shows it
    /// skips and that other rules may be skipped too; and that rc-git cannot undo it.
    pub(crate) fn summary(&self) -> String {
        let merge = &self.merge;
        let short: String = merge.head_oid.to_hex().chars().take(7).collect();
        let known = self.bypassed.known(&merge.base).map_or_else(
            || {
                "GitHub reports it as blocked, but the read names no failed or unfinished \
                 required check, missing review or out-of-date branch."
                    .to_owned()
            },
            |known| format!("Known to be skipped: {known}."),
        );
        format!(
            "Merge {} \u{201c}{}\u{201d} into {} past its branch protection, as an \
             administrator, with {}: {} at {short} ({}). This skips rules set for {} on \
             purpose. {known}{} Branch protection or rulesets may also require things GitHub's \
             API does not report here, and those may be skipped too, so this list may not be \
             complete. GitHub merges it for everyone and rc-git cannot undo it; the local \
             repository is unchanged until you fetch.",
            label(&merge.target, merge.number),
            merge.title,
            merge.base,
            method_phrase(merge.method),
            merge.head,
            checks_phrase(&merge.checks),
            merge.base,
            self.bypassed.uncertain(),
        )
    }
}

/// What stands in the way of an admin merge of `detail` with `method`; `None` when nothing
/// does. The one rule the admin control and the admin merge itself both follow. Only a pull
/// request the plain merge refuses as blocked or behind ([`merge_blocker`]) qualifies, and only
/// when GitHub says the account may merge it as an administrator; every other blocker stands,
/// and a pull request the plain merge can take is [`MergeBlocker::NothingToBypass`].
#[must_use]
pub fn admin_merge_blocker(
    detail: &PullRequestDetail,
    method: MergeMethod,
) -> Option<MergeBlocker> {
    match merge_blocker(detail, method) {
        None => Some(MergeBlocker::NothingToBypass),
        Some(MergeBlocker::Blocked | MergeBlocker::Behind) => {
            let may = detail
                .viewer
                .is_some_and(|viewer| viewer.can_merge_as_admin);
            if !may {
                return Some(MergeBlocker::NoAdminRight);
            }
            detail.id.is_none().then_some(MergeBlocker::NoId)
        }
        Some(
            blocker @ (MergeBlocker::NotOpen(_)
            | MergeBlocker::Draft
            | MergeBlocker::MethodNotAllowed(_)
            | MergeBlocker::Conflicting
            | MergeBlocker::MergeabilityUnknown
            | MergeBlocker::MergeStateUnknown
            | MergeBlocker::MergeStateUnreported
            | MergeBlocker::NoId
            | MergeBlocker::NothingToBypass
            | MergeBlocker::NoAdminRight),
        ) => Some(blocker),
    }
}
