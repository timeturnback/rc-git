//! The words of the Pull Requests section and the pull request tab, decided here so the app
//! and the CLI only place them. A failed read says "couldn't check" and why; what an earlier
//! read left is labelled as possibly out of date, never shown as current.

use rc_git_core::TreeChange;

use super::changes::ChangesPhase;
use super::checks::CheckState;
use super::state::Freshness;
use super::{
    DetailFields, Mergeable, PullRequestStamp, PullRequestState, PullRequestSummary, ReadError,
    ReviewDecision, ServerLink, single_line,
};
use crate::guard_bridge::{FailureKind, classify};
use crate::history_text::{change_path_label, short_hash};
use crate::platform::api_tokens::AccountKey;
use rc_git_guard::actions::ActionError;
use rc_git_guard::actions::github::GitHubHost;

/// The section's heading.
pub const SECTION_HEADING: &str = "Pull Requests";

/// What the section says while its first read runs.
pub const LOADING: &str = "Reading pull requests…";

/// What the section says when the repository has none open.
pub const NO_PULL_REQUESTS: &str = "No open pull requests.";

/// The button that reads the next page.
pub const LOAD_MORE: &str = "Load More";

/// The note over rows a failed refresh left behind.
pub const STALE_NOTE: &str = "Showing what was read before; it may be out of date.";

/// What the tab says while its first read runs.
pub const DETAIL_LOADING: &str = "Reading the pull request…";

/// What the tab says for a description that is empty.
pub const NO_DESCRIPTION: &str = "No description.";

/// What a host older than the full read leaves out, said under the tab's header.
pub const BASIC_FIELDS_NOTE: &str = "This GitHub Enterprise Server does not report the merge \
                                     state, required checks or what your account may do.";

/// The row that offers the sign-in when no account is on `host`.
#[must_use]
pub fn sign_in_row(host: &GitHubHost) -> String {
    format!(
        "Sign in to {host} in Settings > GitHub Accounts to see this repository's pull requests."
    )
}

/// The row that asks which of several accounts on `host` the repository uses.
#[must_use]
pub fn choose_account_row(host: &GitHubHost) -> String {
    format!("Several accounts are signed in to {host}. Choose the one this repository uses:")
}

/// The button that picks `key` for the repository.
#[must_use]
pub fn use_account_label(key: &AccountKey) -> String {
    format!("Use {}", key.login())
}

/// What a failed read says, at `now` (seconds since the Unix epoch).
#[must_use]
pub fn problem(error: &ReadError, now: u64) -> String {
    match error {
        ReadError::SignedOut { account } => format!(
            "Couldn't check: {account} is signed out, or refused its token. Sign in again in \
             Settings > GitHub Accounts."
        ),
        ReadError::RateLimited { reset: Some(reset) } if *reset > now => format!(
            "Couldn't check: the host's rate limit is spent. RCGit checks again in about {} \
             minutes.",
            (reset - now).div_ceil(60)
        ),
        ReadError::RateLimited { .. } => {
            "Couldn't check: the host's rate limit is spent. RCGit checks again later.".to_owned()
        }
        ReadError::Cancelled => "Couldn't check: the read was stopped.".to_owned(),
        ReadError::Failed(cause) => format!("Couldn't check: {cause}"),
    }
}

/// The problem `freshness` says, when it is a failure.
#[must_use]
pub fn freshness_problem(freshness: &Freshness, now: u64) -> Option<String> {
    match freshness {
        Freshness::Failed(error) => Some(problem(error, now)),
        Freshness::Loading | Freshness::Current { .. } => None,
    }
}

/// A row's second line: number, author, branches, draft.
#[must_use]
pub fn row_detail(summary: &PullRequestSummary) -> String {
    let mut line = format!(
        "#{} · {} · {}",
        summary.number,
        author(summary),
        branches(summary)
    );
    if summary.draft {
        line.push_str(" · draft");
    }
    line
}

/// The author's login, or a note for a deleted account.
#[must_use]
pub fn author(summary: &PullRequestSummary) -> String {
    summary
        .author
        .clone()
        .unwrap_or_else(|| "a deleted account".to_owned())
}

/// `head → base`, the head prefixed with its owner when it lives in a fork.
#[must_use]
pub fn branches(summary: &PullRequestSummary) -> String {
    let head = match (&summary.head_owner, summary.cross_repository) {
        (Some(owner), true) => format!("{owner}:{}", summary.head_ref),
        (Some(_) | None, _) => summary.head_ref.clone(),
    };
    format!("{head} → {}", summary.base_ref)
}

/// The tab's title.
#[must_use]
pub fn tab_title(number: u64, summary: Option<&PullRequestSummary>) -> String {
    match summary {
        Some(summary) => format!("#{number} {}", summary.title),
        None => format!("#{number}"),
    }
}

/// The pull request's state.
#[must_use]
pub fn state_label(state: PullRequestState, draft: bool) -> &'static str {
    match (state, draft) {
        (PullRequestState::Open, true) => "Draft",
        (PullRequestState::Open, false) => "Open",
        (PullRequestState::Closed, _) => "Closed",
        (PullRequestState::Merged, _) => "Merged",
        (PullRequestState::Unknown, _) => "Unknown state",
    }
}

/// The review decision.
#[must_use]
pub fn review_label(review: Option<ReviewDecision>) -> &'static str {
    match review {
        Some(ReviewDecision::Approved) => "Approved",
        Some(ReviewDecision::ChangesRequested) => "Changes requested",
        Some(ReviewDecision::ReviewRequired) => "Review required",
        Some(ReviewDecision::Unknown) => "Review state unknown",
        None => "No review required",
    }
}

/// Whether it merges cleanly.
#[must_use]
pub fn mergeable_label(mergeable: Mergeable) -> &'static str {
    match mergeable {
        Mergeable::Mergeable => "No conflicts with the base branch",
        Mergeable::Conflicting => "Conflicts with the base branch",
        Mergeable::Unknown => "GitHub is still checking for conflicts",
    }
}

/// A check's state in words, for screen readers and the CLI.
#[must_use]
pub fn check_label(state: CheckState) -> &'static str {
    match state {
        CheckState::Failing => "failing",
        CheckState::Pending => "pending",
        CheckState::Passing => "passing",
        CheckState::Neutral => "neutral",
    }
}

/// The label of a check's details link, naming the host when it is not the account's.
#[must_use]
pub fn details_label(link: &ServerLink) -> String {
    match link {
        ServerLink::OnHost(_) => "Details".to_owned(),
        ServerLink::Elsewhere { host, .. } => format!("Details on {host}"),
    }
}

/// The note a basic read adds; `None` for a full one.
#[must_use]
pub fn fields_note(fields: DetailFields) -> Option<&'static str> {
    match fields {
        DetailFields::Full => None,
        DetailFields::Basic => Some(BASIC_FIELDS_NOTE),
    }
}

/// The tab's first view: header, checks and description.
pub const OVERVIEW_VIEW: &str = "Overview";

/// The tab's second view: the changed files and their diffs.
pub const CHANGES_VIEW: &str = "Changes";

/// The button that fetches a pull request's head.
pub const FETCH_CHANGES: &str = "Fetch Changes";

/// The button that stops a running fetch.
pub const STOP_FETCH: &str = "Stop";

/// The button that reads the local state again.
pub const LOOK_AGAIN: &str = "Look Again";

/// What the changes view says while it reads the repository.
pub const CHANGES_READING: &str = "Reading the changes…";

/// What the diff pane says before a file is picked.
pub const PICK_A_FILE: &str = "Select a file to see its changes.";

/// What the changes view says for `phase`, the changes of `stamp` read from `remote`.
#[must_use]
pub fn changes_note(phase: &ChangesPhase, stamp: &PullRequestStamp, remote: &str) -> String {
    let (number, head) = (stamp.number, short_hash(&stamp.head_oid));
    match phase {
        ChangesPhase::Reading => CHANGES_READING.to_owned(),
        ChangesPhase::NeedsFetch { problem: None } => format!(
            "Commit {head}, the head of #{number}, is not in this repository. Fetch Changes \
             downloads it from {remote} into refs/rc-git/pr/{number}/head; no branch changes and \
             nothing is checked out."
        ),
        ChangesPhase::NeedsFetch {
            problem: Some(problem),
        } => format!("The fetch failed: {problem}"),
        ChangesPhase::Fetching => format!("Fetching #{number} ({head}) from {remote}…"),
        ChangesPhase::Moved { found } => format!(
            "#{number} moved after it was read: {remote} has {} as its head, not {head}. \
             Nothing was fetched; reading the pull request again.",
            short_hash(found)
        ),
        ChangesPhase::NoBaseCopy { branch, tracking } => match tracking {
            Some(tracking) => format!(
                "There is no {tracking} here, the copy of {branch} the changes are compared \
                 with. Fetch {remote} (Fetch in the toolbar), then Look Again."
            ),
            None => format!(
                "{remote}'s fetch settings keep no copy of {branch}, the branch the changes are \
                 compared with. Fetch it with git, then Look Again."
            ),
        },
        ChangesPhase::NoMergeBase {
            tracking,
            incomplete: true,
            scanned,
        } => format!(
            "rc-git read {scanned} commits without finding where {head} branches off \
             {tracking}. The history here may be shallow; fetch more of it with git, then Look \
             Again."
        ),
        ChangesPhase::NoMergeBase {
            tracking,
            incomplete: false,
            ..
        } => format!("{head} and {tracking} share no history, so there is nothing to compare."),
        ChangesPhase::Ready {
            base,
            tracking,
            files,
        } => format!(
            "{} in {head} since {} (where it branches off {tracking})",
            changed_files(files.len()),
            short_hash(base)
        ),
        ChangesPhase::Failed(message) => format!("Couldn't read the changes: {message}"),
    }
}

/// The longest changed-file label kept, in characters.
const MAX_PATH_CHARS: usize = 4096;

/// A changed file's label in the Changes view: its path (`old → new` for a rename) with
/// control, bidi and zero-width characters shown as escapes, since whoever opened the pull
/// request named the file.
#[must_use]
pub fn changed_file_label(change: &TreeChange) -> String {
    single_line(&change_path_label(change), MAX_PATH_CHARS)
}

/// `n file(s) changed`.
fn changed_files(count: usize) -> String {
    match count {
        1 => "1 file changed".to_owned(),
        count => format!("{count} files changed"),
    }
}

/// What a failed head fetch says, with what to do when a secret or an approval saved for
/// `host` would fix it. The fetch offers only what is saved; it never prompts.
#[must_use]
pub fn fetch_problem(error: &ActionError, host: &str) -> String {
    let message = error.to_string();
    match classify(error) {
        FailureKind::AuthRequired | FailureKind::PassphraseRequired { .. } => format!(
            "{message}. The fetch offers only the credentials saved for {host}: use Fetch in the \
             toolbar once to enter them, then fetch the pull request again."
        ),
        FailureKind::HostKeyUnknown { .. } => format!(
            "{message}. Approve {host}'s host key with Fetch in the toolbar first, then fetch \
             the pull request again."
        ),
        FailureKind::IdentityRequired
        | FailureKind::Stale
        | FailureKind::CredentialHostRefused
        | FailureKind::Diverged
        | FailureKind::Cancelled
        | FailureKind::Failed => message,
    }
}
