//! The words of the Pull Requests section and the pull request tab, decided here so the app
//! and the CLI only place them. A failed read says "couldn't check" and why; what an earlier
//! read left is labelled as possibly out of date, never shown as current.

use rc_git_core::TreeChange;

use super::changes::ChangesPhase;
use super::checks::CheckState;
use super::merge::{MergeEnd, MergeFlow, MergePhase, MergeStop};
use super::state::Freshness;
use super::{
    AfterLoss, Bypassed, DetailFields, MergeBlocker, MergeKind, MergeMethod, Mergeable,
    PullRequestStamp, PullRequestState, PullRequestSummary, ReadError, ReviewDecision, ServerLink,
    Settled, single_line,
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

/// The Merge control's button, and the title and confirm button of its challenge.
pub const MERGE: &str = "Merge";

/// Why Merge takes no click while the repository's account is signed out.
pub const MERGE_SIGNED_OUT: &str =
    "Can't merge: sign in to the repository's host in Settings > GitHub Accounts first.";

/// Why Merge takes no click when the repository allows no method.
pub const MERGE_NO_METHOD: &str = "Can't merge: the repository allows no merge method.";

/// The admin merge control's button: worded so it never reads as Merge.
pub const ADMIN_MERGE: &str = "Merge Without Waiting for Requirements (Admin)";

/// The admin merge challenge's title and confirm button.
pub const ADMIN_MERGE_TITLE: &str = "Merge Past Branch Protection";

/// The line beside the admin merge control.
pub const ADMIN_MERGE_NOTE: &str = "GitHub says your account may merge this past the branch \
     protection that blocks it. That skips rules set for the base on purpose: the next step lists \
     what is known to be skipped and asks you to type the pull request's number.";

/// The button that stops a merge whose request has not left.
pub const MERGE_CANCEL: &str = "Cancel";

/// The button that clears a finished merge's note.
pub const MERGE_DISMISS: &str = "Dismiss";

/// Why Merge takes no click for `blocker`.
#[must_use]
pub fn merge_blocked(blocker: &MergeBlocker) -> String {
    format!("Can't merge: {blocker}.")
}

/// A merge method as the control offers it, in GitHub's own words.
#[must_use]
pub fn merge_method_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "Squash and Merge",
        MergeMethod::Merge => "Create a Merge Commit",
        MergeMethod::Rebase => "Rebase and Merge",
    }
}

/// A merge method as one lowercase word, for the CLI and the log.
#[must_use]
pub fn merge_method_word(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "squash",
        MergeMethod::Merge => "merge",
        MergeMethod::Rebase => "rebase",
    }
}

/// What the tab says about a merge that sent nothing.
#[must_use]
pub fn merge_stop_message(stop: &MergeStop) -> String {
    match stop {
        MergeStop::Cancelled => "Cancelled; nothing was sent.".to_owned(),
        MergeStop::Refused(refusal) => format!("{refusal}."),
        MergeStop::SignedOut { account } => format!(
            "Nothing was sent: {account} is signed out, or refused its token. Sign in again in \
             Settings > GitHub Accounts."
        ),
        MergeStop::Failed(cause) => format!("Nothing was sent: {cause}"),
    }
}

/// A merge of `kind` as the notes and the log name it.
fn merge_word(kind: MergeKind) -> &'static str {
    match kind {
        MergeKind::Plain => "merge",
        MergeKind::Admin => "admin merge",
    }
}

/// What the tab says under the Merge control for `flow`; `None` while the challenge is up,
/// since the sheet says it.
#[must_use]
pub fn merge_note(flow: &MergeFlow) -> Option<String> {
    let number = flow.number();
    let word = merge_word(flow.kind());
    let note = match flow.phase() {
        MergePhase::Planning => format!("Reading #{number} again before the {word}…"),
        MergePhase::Confirming(_) => return None,
        MergePhase::Merging {
            sent: false,
            cancelling: false,
        } => format!("Merging #{number}… Cancel stops it until the request is sent."),
        MergePhase::Merging {
            sent: false,
            cancelling: true,
        } => format!("Stopping the {word} of #{number}…"),
        MergePhase::Merging { sent: true, .. } => format!(
            "The {word} request for #{number} was sent; it cannot be stopped now. Waiting for \
             GitHub's answer…"
        ),
        MergePhase::Done(MergeEnd::Refused { reason }) if flow.kind() == MergeKind::Admin => {
            admin_refused_note(number, reason)
        }
        MergePhase::Done(end) => merge_end_note(number, &flow.stamp().base_ref, end),
    };
    Some(note)
}

/// What an admin merge GitHub refused says: its reason, and what rc-git does not know about
/// which sign-in carries the right to merge past branch protection.
fn admin_refused_note(number: u64, reason: &str) -> String {
    format!(
        "GitHub refused the admin merge of #{number}: {reason} Nothing was merged. GitHub alone \
         decides who may merge past branch protection, and rc-git has not verified that a \
         GitHub App sign-in carries an administrator's bypass right: if this account signed in \
         through a GitHub App, signing in through an OAuth App or with a personal access token \
         may be needed."
    )
}

/// What a finished merge says.
fn merge_end_note(number: u64, base: &str, end: &MergeEnd) -> String {
    const UNCHANGED: &str = "The local repository is unchanged until you fetch.";
    const LOOK: &str = "Look at the pull request on GitHub before trying again.";
    match end {
        MergeEnd::NotSent(stop) => merge_stop_message(stop),
        MergeEnd::Merged {
            merge_commit,
            settled,
        } => {
            let commit = merge_commit
                .as_ref()
                .map_or_else(String::new, |oid| format!(" as {}", short_hash(oid)));
            let how = match settled {
                Settled::Answer => String::new(),
                Settled::Reread => format!(
                    " GitHub's answer was lost; reading #{number} again shows it merged at the \
                     head you confirmed."
                ),
            };
            format!("Merged #{number} into {base}{commit}.{how} {UNCHANGED}")
        }
        MergeEnd::Refused { reason } => {
            format!("GitHub refused the merge of #{number}: {reason} Nothing was merged.")
        }
        MergeEnd::Unknown { problem, after } => match after {
            AfterLoss::StillOpen { .. } => format!(
                "rc-git could not tell whether #{number} merged: GitHub did not answer \
                 ({problem}). Reading it again shows it still open, but the merge may still \
                 land. {LOOK}"
            ),
            AfterLoss::Closed => format!(
                "#{number} did not merge: GitHub did not answer ({problem}), and reading it \
                 again shows it closed."
            ),
            AfterLoss::MergedAtOtherHead { head } => format!(
                "#{number} is merged, but at {}, not the head you confirmed, so not by this \
                 request. {LOOK}",
                short_hash(head)
            ),
            AfterLoss::Unread { reason } => format!(
                "rc-git could not tell whether #{number} merged: GitHub did not answer \
                 ({problem}), and reading it again failed too ({reason}). {LOOK}"
            ),
        },
    }
}

/// The app log's line for a merge of `repository#number` at `head` with `method` that is about
/// to be sent, with what an admin merge skips: no token, no server text but the guard's
/// scrubbed messages and neutralized check names.
#[must_use]
pub fn merge_attempt_log(repository: &str, flow: &MergeFlow) -> String {
    format!("{}: sending", log_subject(repository, flow))
}

/// `merge owner/name#7 head <oid> method squash`, and for an admin merge
/// `admin merge … bypassing <what>`.
fn log_subject(repository: &str, flow: &MergeFlow) -> String {
    let subject = format!(
        "{} {repository}#{} head {} method {}",
        merge_word(flow.kind()),
        flow.number(),
        flow.stamp().head_oid,
        merge_method_word(flow.method())
    );
    match flow.kind() {
        MergeKind::Plain => subject,
        MergeKind::Admin => format!("{subject} bypassing {}", bypassed_log(flow.bypassed())),
    }
}

/// What an admin merge skips, for the log: every list in full, names as the guard neutralized
/// them.
fn bypassed_log(bypassed: Option<&Bypassed>) -> String {
    let Some(bypassed) = bypassed else {
        return "[not read]".to_owned();
    };
    let review = match bypassed.review {
        Some(ReviewDecision::ReviewRequired) => "review required",
        Some(ReviewDecision::ChangesRequested) => "changes requested",
        Some(ReviewDecision::Approved | ReviewDecision::Unknown) | None => "none",
    };
    format!(
        "[failed required checks: {:?}; pending required checks: {:?}; checks of unreported \
         requirement not passed: {}; checks not read: {}; review: {review}; behind base: {}]",
        bypassed.failed, bypassed.pending, bypassed.unreported, bypassed.unread, bypassed.behind
    )
}

/// The app log's line for how a merge of `repository` ended.
#[must_use]
pub fn merge_outcome_log(repository: &str, flow: &MergeFlow, end: &MergeEnd) -> String {
    let outcome = match end {
        MergeEnd::NotSent(stop) => format!("not sent: {}", merge_stop_message(stop)),
        MergeEnd::Merged {
            merge_commit,
            settled,
        } => {
            let commit = merge_commit.map_or_else(|| "-".to_owned(), |oid| oid.to_hex());
            let how = match settled {
                Settled::Answer => "answer",
                Settled::Reread => "read after a lost answer",
            };
            format!("merged as {commit} ({how})")
        }
        MergeEnd::Refused { reason } => format!("refused: {reason}"),
        MergeEnd::Unknown { problem, after } => format!("unknown: {problem}; after: {after:?}"),
    };
    format!("{}: {outcome}", log_subject(repository, flow))
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
