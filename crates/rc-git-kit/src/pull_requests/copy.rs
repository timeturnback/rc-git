//! The words of the Pull Requests section and the pull request tab, decided here so the app
//! and the CLI only place them. A failed read says "couldn't check" and why; what an earlier
//! read left is labelled as possibly out of date, never shown as current.

use rc_git_core::{Oid, TreeChange};

use super::auto_merge::{AutoMergeEnd, AutoMergeFlow};
use super::changes::{ChangesPhase, ComparedWith};
use super::checks::CheckState;
use super::delete_branch::{DeleteBranchEnd, DeleteBranchFlow};
use super::flow::{Ended, Phase, Stop, TabFlow};
use super::logs::{LogPhase, LogState, LogStop};
use super::merge::{MergeEnd, MergeFlow, MergePhase, MergeStop};
use super::rerun::{RerunEnd, RerunFlow};
use super::state::Freshness;
use super::{
    AfterLoss, AutoMergeAfter, AutoMergeChange, AutoMergeOutcome, BranchAfter, BranchDeleteOutcome,
    BranchDeleteReport, Bypassed, DetailFields, LOG_LINE_CHARS, LogCut, MAX_RERUN_RUNS,
    MergeBlocker, MergeKind, MergeMethod, Mergeable, PullRequestStamp, PullRequestState,
    PullRequestSummary, ReadError, RerunReport, ReviewDecision, RunOutcome, ServerLink, Settled,
    failed_runs, single_line,
};
use crate::guard_bridge::{FailureKind, classify};
use crate::history_text::{change_path_label, short_hash};
use crate::platform::api_tokens::AccountKey;
use crate::text_document::{format_bytes, grouped};
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
        ChangesPhase::NoBaseCopy {
            branch,
            tracking,
            commit,
        } => no_base_note(branch, tracking.as_deref(), commit.as_ref(), remote),
        ChangesPhase::NoMergeBase {
            compared,
            incomplete: true,
            scanned,
        } => format!(
            "rc-git read {scanned} commits without finding where {head} branches off {}. The \
             history here may be shallow; fetch more of it with git, then Look Again.",
            compared_label(compared, &stamp.base_ref)
        ),
        ChangesPhase::NoMergeBase {
            compared,
            incomplete: false,
            ..
        } => format!(
            "{head} and {} share no history, so there is nothing to compare.",
            compared_label(compared, &stamp.base_ref)
        ),
        ChangesPhase::Ready {
            base,
            compared: ComparedWith::PullRequestBase(commit),
            files,
        } => format!(
            "{} in {head} since {}, where it branches off {} at {}, the base GitHub reports",
            changed_files(files.len()),
            short_hash(base),
            stamp.base_ref,
            short_hash(commit)
        ),
        ChangesPhase::Ready {
            base,
            compared: ComparedWith::LocalCopy(tracking),
            files,
        } => format!(
            "{} in {head} since {}, where it branches off {tracking}, the local copy of {}. \
             This host does not report the pull request's base commit, so GitHub may show \
             other changes for #{number}.",
            changed_files(files.len()),
            short_hash(base),
            stamp.base_ref
        ),
        ChangesPhase::Failed(message) => format!("Couldn't read the changes: {message}"),
    }
}

/// What the head of a pull request based on `branch` was compared with, in words.
fn compared_label(compared: &ComparedWith, branch: &str) -> String {
    match compared {
        ComparedWith::PullRequestBase(commit) => {
            format!("{branch} at {}", short_hash(commit))
        }
        ComparedWith::LocalCopy(tracking) => format!("{tracking}, the local copy of {branch}"),
    }
}

/// What the changes view says when nothing local can stand for the base: the base commit
/// GitHub reported (`commit`) is missing, or the host reported none and `tracking`, the copy of
/// `branch` the remote's fetch settings name (`None`: they name none), is missing too.
fn no_base_note(
    branch: &str,
    tracking: Option<&str>,
    commit: Option<&Oid>,
    remote: &str,
) -> String {
    let what = match commit {
        Some(commit) => format!(
            "Commit {}, where {branch} is on GitHub, is not in this repository, and the changes \
             are compared with it.",
            short_hash(commit)
        ),
        None => {
            format!("There is no copy of {branch} here, the branch the changes are compared with.")
        }
    };
    match tracking {
        Some(_) => format!("{what} Fetch {remote} (Fetch in the toolbar), then Look Again."),
        None => format!(
            "{what} {remote}'s fetch settings keep no copy of {branch}, so fetch it with git, \
             then Look Again."
        ),
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
    stop_message(stop)
}

/// What the tab says about a guarded action that sent nothing.
#[must_use]
pub fn stop_message<R: std::fmt::Display>(stop: &Stop<R>) -> String {
    match stop {
        Stop::Cancelled => "Cancelled; nothing was sent.".to_owned(),
        Stop::Refused(refusal) => format!("{refusal}."),
        Stop::SignedOut { account } => format!(
            "Nothing was sent: {account} is signed out, or refused its token. Sign in again in \
             Settings > GitHub Accounts."
        ),
        Stop::Failed(cause) => format!("Nothing was sent: {cause}"),
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
        MergePhase::Confirming => return None,
        MergePhase::Sending {
            sent: false,
            cancelling: false,
        } => format!("Merging #{number}… Cancel stops it until the request is sent."),
        MergePhase::Sending {
            sent: false,
            cancelling: true,
        } => format!("Stopping the {word} of #{number}…"),
        MergePhase::Sending { sent: true, .. } => format!(
            "The {word} request for #{number} was sent; it cannot be stopped now. Waiting for \
             GitHub's answer…"
        ),
        MergePhase::Done(MergeEnd::Refused { reason }) if flow.kind() == MergeKind::Admin => {
            admin_refused_note(number, reason)
        }
        MergePhase::Done(end) => merge_end_note(number, end),
    };
    Some(note)
}

/// Whether `flow`'s note is an alarm the tab shows in red: GitHub refused the merge, rc-git
/// cannot tell whether it merged, or it merged into another base than the one confirmed.
#[must_use]
pub fn merge_note_alarms(flow: &MergeFlow) -> bool {
    match flow.phase() {
        MergePhase::Done(
            MergeEnd::Refused { .. }
            | MergeEnd::Unknown { .. }
            | MergeEnd::MergedIntoOtherBase { .. },
        ) => true,
        MergePhase::Planning
        | MergePhase::Confirming
        | MergePhase::Sending { .. }
        | MergePhase::Done(MergeEnd::Merged { .. } | MergeEnd::NotSent(_)) => false,
    }
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
fn merge_end_note(number: u64, end: &MergeEnd) -> String {
    const UNCHANGED: &str = "The local repository is unchanged until you fetch.";
    const LOOK: &str = "Look at the pull request on GitHub before trying again.";
    let commit = |merge_commit: &Option<Oid>| {
        merge_commit
            .as_ref()
            .map_or_else(String::new, |oid| format!(" as {}", short_hash(oid)))
    };
    let how = |settled: &Settled| match settled {
        Settled::Answer => String::new(),
        Settled::Reread => format!(
            " GitHub's answer was lost; reading #{number} again shows it merged at the head you \
             confirmed."
        ),
    };
    match end {
        MergeEnd::NotSent(stop) => merge_stop_message(stop),
        MergeEnd::Merged {
            merge_commit,
            base: Some(base),
            settled,
        } => format!(
            "Merged #{number} into {base}{}.{} {UNCHANGED}",
            commit(merge_commit),
            how(settled)
        ),
        MergeEnd::Merged {
            merge_commit,
            base: None,
            settled,
        } => format!(
            "Merged #{number}{}. GitHub did not say which branch it merged into; look at the \
             pull request on GitHub to see.{} {UNCHANGED}",
            commit(merge_commit),
            how(settled)
        ),
        MergeEnd::MergedIntoOtherBase {
            confirmed,
            actual,
            merge_commit,
            settled,
        } => format!(
            "Warning: #{number} merged into {actual}, not {confirmed}, the base you confirmed{}. \
             Its base was changed on GitHub after rc-git last read it, and GitHub's merge cannot \
             refuse that. Look at {actual} on GitHub now.{} {UNCHANGED}",
            commit(merge_commit),
            how(settled)
        ),
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

/// The app log's line for `flow`'s merge, about to be sent: its repository (as the flow started
/// with it), number, head and method, with what an admin merge skips. No token, no server text
/// but the guard's scrubbed messages and neutralized check names.
#[must_use]
pub fn merge_attempt_log(flow: &MergeFlow) -> String {
    format!("{}: sending", log_subject(flow))
}

/// `merge #7 of owner/name@host head <oid> method squash`, and for an admin merge
/// `admin merge … bypassing <what>`.
fn log_subject(flow: &MergeFlow) -> String {
    let subject = format!(
        "{} #{} of {} head {} method {}",
        merge_word(flow.kind()),
        flow.number(),
        flow.repository(),
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

/// The app log's line for how `flow`'s merge ended.
#[must_use]
pub fn merge_outcome_log(flow: &MergeFlow, end: &MergeEnd) -> String {
    let commit = |merge_commit: &Option<Oid>| {
        merge_commit.map_or_else(|| "-".to_owned(), |oid| oid.to_hex())
    };
    let how = |settled: &Settled| match settled {
        Settled::Answer => "answer",
        Settled::Reread => "read after a lost answer",
    };
    let outcome = match end {
        MergeEnd::NotSent(stop) => format!("not sent: {}", merge_stop_message(stop)),
        MergeEnd::Merged {
            merge_commit,
            base,
            settled,
        } => format!(
            "merged as {} into {} ({})",
            commit(merge_commit),
            base.as_deref().unwrap_or("a base GitHub did not name"),
            how(settled)
        ),
        MergeEnd::MergedIntoOtherBase {
            confirmed,
            actual,
            merge_commit,
            settled,
        } => format!(
            "MERGED INTO ANOTHER BASE: merged as {} into {actual}, not the confirmed {confirmed} \
             ({})",
            commit(merge_commit),
            how(settled)
        ),
        MergeEnd::Refused { reason } => format!("refused: {reason}"),
        MergeEnd::Unknown { problem, after } => format!("unknown: {problem}; after: {after:?}"),
    };
    format!("{}: {outcome}", log_subject(flow))
}

/// The button that turns auto-merge on, and the title and confirm button of its challenge.
pub const ENABLE_AUTO_MERGE: &str = "Enable Auto-Merge";

/// The button that turns auto-merge off, and the title and confirm button of its challenge.
pub const DISABLE_AUTO_MERGE: &str = "Disable Auto-Merge";

/// The line beside the button that turns auto-merge on.
pub const AUTO_MERGE_NOTE: &str = "Auto-merge lets GitHub merge this pull request by itself once \
     its requirements are met, with any commits pushed to its head branch by then, without asking \
     again. The next step shows what you are turning on.";

/// The button that re-runs failed jobs, and the title and confirm button of its challenge.
pub const RERUN_FAILED_JOBS: &str = "Re-run Failed Jobs";

/// The line under the auto-merge control while auto-merge is on, with the `method` it merges
/// with and who turned it on, as the read reported them.
#[must_use]
pub fn auto_merge_on_line(method: Option<MergeMethod>, enabled_by: Option<&str>) -> String {
    let how = method.map_or("a method rc-git does not know", merge_method_phrase);
    let by = enabled_by.map_or_else(String::new, |login| format!(" Turned on by {login}."));
    format!(
        "Auto-merge is on: GitHub merges this pull request by itself as {how} once its \
         requirements are met, with any commits pushed to its head branch by then.{by}"
    )
}

/// The line beside the re-run control: how many runs one confirmation re-runs, and how many it
/// leaves for another.
#[must_use]
pub fn rerun_line(runs: usize, left_out: usize) -> String {
    let runs = match runs {
        1 => "1 GitHub Actions run".to_owned(),
        runs => format!("{runs} GitHub Actions runs"),
    };
    let left = if left_out > 0 {
        format!(
            " {left_out} more wait for another confirmation (at most {MAX_RERUN_RUNS} at once)."
        )
    } else {
        String::new()
    };
    format!("Re-runs the failed jobs of {runs} of this head.{left}")
}

/// `method` as the challenge words it.
fn merge_method_phrase(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "a merge commit",
        MergeMethod::Squash => "a squash merge",
        MergeMethod::Rebase => "a rebase merge",
    }
}

/// The title and confirm button of `flow`'s challenge.
#[must_use]
pub fn flow_title(flow: &TabFlow) -> &'static str {
    match flow {
        TabFlow::Merge(merge) => match merge.kind() {
            MergeKind::Plain => MERGE,
            MergeKind::Admin => ADMIN_MERGE_TITLE,
        },
        TabFlow::AutoMerge(auto) => match auto.ask().0 {
            AutoMergeChange::Enable(_) => ENABLE_AUTO_MERGE,
            AutoMergeChange::Disable => DISABLE_AUTO_MERGE,
        },
        TabFlow::Rerun(_) => RERUN_FAILED_JOBS,
        TabFlow::DeleteBranch(_) => DELETE_BRANCH,
    }
}

/// What the tab says under its controls for `flow`; `None` while the challenge is up.
#[must_use]
pub fn flow_note(flow: &TabFlow) -> Option<String> {
    match flow {
        TabFlow::Merge(merge) => merge_note(merge),
        TabFlow::AutoMerge(auto) => auto_merge_note(auto),
        TabFlow::Rerun(rerun) => rerun_note(rerun),
        TabFlow::DeleteBranch(delete) => delete_branch_note(delete),
    }
}

/// Whether `flow`'s note is an alarm the tab shows in red.
#[must_use]
pub fn flow_note_alarms(flow: &TabFlow) -> bool {
    match flow {
        TabFlow::Merge(merge) => merge_note_alarms(merge),
        TabFlow::AutoMerge(auto) => matches!(
            auto.phase(),
            Phase::Done(Ended::Sent(
                AutoMergeOutcome::Refused { .. }
                    | AutoMergeOutcome::Unknown { .. }
                    | AutoMergeOutcome::OnOtherBase { .. }
            ))
        ),
        TabFlow::Rerun(rerun) => matches!(
            rerun.phase(),
            Phase::Done(Ended::Sent(report)) if report.runs.iter().any(|run| run.outcome != RunOutcome::Requested)
        ),
        TabFlow::DeleteBranch(delete) => matches!(
            delete.phase(),
            Phase::Done(Ended::Sent(report)) if !matches!(report.outcome, BranchDeleteOutcome::Deleted { .. })
        ),
    }
}

/// `on` or `off`, as `change` turns auto-merge.
fn turns(change: AutoMergeChange) -> &'static str {
    match change {
        AutoMergeChange::Enable(_) => "on",
        AutoMergeChange::Disable => "off",
    }
}

/// What the tab says for an auto-merge change; `None` while the challenge is up.
#[must_use]
pub fn auto_merge_note(flow: &AutoMergeFlow) -> Option<String> {
    let number = flow.number();
    let way = turns(flow.ask().0);
    let note = match flow.phase() {
        Phase::Planning => format!("Reading #{number} again before turning auto-merge {way}…"),
        Phase::Confirming => return None,
        Phase::Sending {
            sent: false,
            cancelling: false,
        } => format!(
            "Turning auto-merge {way} for #{number}… Cancel stops it until the request is sent."
        ),
        Phase::Sending {
            sent: false,
            cancelling: true,
        } => format!("Stopping the auto-merge change of #{number}…"),
        Phase::Sending { sent: true, .. } => format!(
            "The request to turn auto-merge {way} for #{number} was sent; it cannot be stopped \
             now. Waiting for GitHub's answer…"
        ),
        Phase::Done(end) => auto_merge_end_note(number, flow.ask().0, end),
    };
    Some(note)
}

/// What a finished auto-merge change says.
fn auto_merge_end_note(number: u64, change: AutoMergeChange, end: &AutoMergeEnd) -> String {
    const LOOK: &str = "Look at the pull request on GitHub before trying again.";
    let way = turns(change);
    let how = |settled: &Settled| match settled {
        Settled::Answer => String::new(),
        Settled::Reread => {
            format!(" GitHub's answer was lost; reading #{number} again shows auto-merge {way}.")
        }
    };
    let outcome = match end {
        Ended::NotSent(stop) => return stop_message(stop),
        Ended::Sent(outcome) => outcome,
    };
    match outcome {
        AutoMergeOutcome::Done { settled } => match change {
            AutoMergeChange::Enable(_) => format!(
                "Auto-merge is on for #{number}: GitHub merges it by itself once its \
                 requirements are met, with any commits pushed to its head branch by then.{}",
                how(settled)
            ),
            AutoMergeChange::Disable => format!(
                "Auto-merge is off for #{number}: GitHub no longer merges it by itself.{}",
                how(settled)
            ),
        },
        AutoMergeOutcome::OnOtherBase {
            confirmed,
            actual,
            settled,
        } => format!(
            "Warning: auto-merge is on for #{number}, but its base is now {actual}, not \
             {confirmed}, the base you confirmed: it will merge into {actual}. Its base was \
             changed on GitHub after rc-git last read it. Turn auto-merge off if that is not what \
             you want.{}",
            how(settled)
        ),
        AutoMergeOutcome::Refused { reason } => format!(
            "GitHub refused to turn auto-merge {way} for #{number}: {reason} Nothing changed."
        ),
        AutoMergeOutcome::Unknown { problem, after } => format!(
            "rc-git could not tell whether auto-merge is {way} for #{number}: GitHub did not \
             answer ({problem}), and reading it again shows {}. {LOOK}",
            after_phrase(after)
        ),
    }
}

/// What the read after a lost auto-merge answer showed.
fn after_phrase(after: &AutoMergeAfter) -> String {
    match after {
        AutoMergeAfter::On { method } => format!(
            "auto-merge on, as {}",
            method.map_or("a method rc-git does not know", merge_method_phrase)
        ),
        AutoMergeAfter::Off => "auto-merge off".to_owned(),
        AutoMergeAfter::OtherHead { head } => {
            format!("it open at another head, {}", short_hash(head))
        }
        AutoMergeAfter::Merged => "it merged".to_owned(),
        AutoMergeAfter::Closed => "it closed".to_owned(),
        AutoMergeAfter::Unread { reason } => format!("nothing: that read failed too ({reason})"),
    }
}

/// What the tab says for a re-run; `None` while the challenge is up.
#[must_use]
pub fn rerun_note(flow: &RerunFlow) -> Option<String> {
    let number = flow.number();
    let note = match flow.phase() {
        Phase::Planning => format!("Reading #{number} again before re-running its failed jobs…"),
        Phase::Confirming => return None,
        Phase::Sending {
            sent: false,
            cancelling: false,
        } => format!(
            "Asking GitHub to re-run the failed jobs of #{number}… Cancel stops it until the \
             first request is sent."
        ),
        Phase::Sending {
            sent: false,
            cancelling: true,
        } => format!("Stopping the re-run of #{number}…"),
        Phase::Sending { sent: true, .. } => format!(
            "The re-run requests for #{number} are being sent; they cannot be stopped now. \
             Waiting for GitHub's answers…"
        ),
        Phase::Done(Ended::NotSent(stop)) => stop_message(stop),
        Phase::Done(Ended::Sent(report)) => rerun_report_note(number, report),
    };
    Some(note)
}

/// What a re-run whose requests went says: every run and how its request ended.
fn rerun_report_note(number: u64, report: &RerunReport) -> String {
    if report
        .runs
        .iter()
        .all(|run| run.outcome == RunOutcome::Requested)
    {
        let ids: Vec<String> = report.runs.iter().map(|run| run.run.to_string()).collect();
        return format!(
            "GitHub re-runs the failed jobs of #{number}'s runs {}. The checks show their \
             progress as they run.",
            ids.join(", ")
        );
    }
    let mut lines = Vec::new();
    for run in &report.runs {
        lines.push(format!(
            "Run {}: {}",
            run.run,
            run_outcome_phrase(&run.outcome)
        ));
    }
    format!(
        "Re-running the failed jobs of #{number}: {} Look at the runs on GitHub before asking \
         again; rc-git never sends a re-run twice.",
        lines.join(" ")
    )
}

/// How one run's request ended, as a sentence.
fn run_outcome_phrase(outcome: &RunOutcome) -> String {
    match outcome {
        RunOutcome::Requested => "re-run asked for.".to_owned(),
        RunOutcome::Refused { reason } => format!("GitHub refused: {reason}."),
        RunOutcome::Unknown { problem } => {
            format!("rc-git could not tell whether GitHub re-runs it ({problem}).")
        }
        RunOutcome::NotSent { reason } => format!("not sent ({reason})."),
        RunOutcome::NotMatched { reason } => format!(
            "could not be matched to the pull request's head commit, so it was not sent \
             ({reason})."
        ),
    }
}

/// `turn on auto-merge #7 of owner/name@host head <oid> method squash`.
fn auto_merge_subject(flow: &AutoMergeFlow) -> String {
    let what = match flow.ask().0 {
        AutoMergeChange::Enable(method) => {
            format!(
                "turn on auto-merge with method {}",
                merge_method_word(method)
            )
        }
        AutoMergeChange::Disable => "turn off auto-merge".to_owned(),
    };
    format!(
        "{what} #{} of {} head {}",
        flow.number(),
        flow.repository(),
        flow.stamp().head_oid
    )
}

/// The app log's line for `flow`'s auto-merge change, about to be sent. No token, no server
/// text but the guard's scrubbed messages.
#[must_use]
pub fn auto_merge_attempt_log(flow: &AutoMergeFlow) -> String {
    format!("{}: sending", auto_merge_subject(flow))
}

/// The app log's line for how `flow`'s auto-merge change ended.
#[must_use]
pub fn auto_merge_outcome_log(flow: &AutoMergeFlow, end: &AutoMergeEnd) -> String {
    let how = |settled: &Settled| match settled {
        Settled::Answer => "answer",
        Settled::Reread => "read after a lost answer",
    };
    let way = turns(flow.ask().0);
    let outcome = match end {
        Ended::NotSent(stop) => format!("not sent: {}", stop_message(stop)),
        Ended::Sent(AutoMergeOutcome::Done { settled }) => format!("{way} ({})", how(settled)),
        Ended::Sent(AutoMergeOutcome::OnOtherBase {
            confirmed,
            actual,
            settled,
        }) => format!(
            "ON FOR ANOTHER BASE: on for {actual}, not the confirmed {confirmed} ({})",
            how(settled)
        ),
        Ended::Sent(AutoMergeOutcome::Refused { reason }) => format!("refused: {reason}"),
        Ended::Sent(AutoMergeOutcome::Unknown { problem, after }) => {
            format!("unknown: {problem}; after: {after:?}")
        }
    };
    format!("{}: {outcome}", auto_merge_subject(flow))
}

/// `re-run failed jobs #7 of owner/name@host head <oid> runs [41, 42]`: the runs the plan's
/// read names, in the order the guard sends them.
fn rerun_subject(flow: &RerunFlow) -> String {
    let runs = flow.planned_as().map_or_else(
        || "[not read]".to_owned(),
        |plan| {
            let ids: Vec<String> = failed_runs(&plan.detail)
                .runs
                .iter()
                .take(MAX_RERUN_RUNS)
                .map(|run| run.id.to_string())
                .collect();
            format!("[{}]", ids.join(", "))
        },
    );
    format!(
        "re-run failed jobs #{} of {} head {} runs {runs}",
        flow.number(),
        flow.repository(),
        flow.stamp().head_oid
    )
}

/// The app log's line for `flow`'s re-run, about to be sent.
#[must_use]
pub fn rerun_attempt_log(flow: &RerunFlow) -> String {
    format!("{}: sending", rerun_subject(flow))
}

/// The app log's line for how `flow`'s re-run ended: every run and its outcome.
#[must_use]
pub fn rerun_outcome_log(flow: &RerunFlow, end: &RerunEnd) -> String {
    let outcome = match end {
        Ended::NotSent(stop) => format!("not sent: {}", stop_message(stop)),
        Ended::Sent(report) => {
            let mut runs = Vec::new();
            for run in &report.runs {
                let what = match &run.outcome {
                    RunOutcome::Requested => "requested".to_owned(),
                    RunOutcome::Refused { reason } => format!("refused: {reason}"),
                    RunOutcome::Unknown { problem } => format!("unknown: {problem}"),
                    RunOutcome::NotSent { reason } => format!("not sent: {reason}"),
                    RunOutcome::NotMatched { reason } => format!("not matched: {reason}"),
                };
                runs.push(format!("run {} {what}", run.run));
            }
            runs.join("; ")
        }
    };
    format!("{}: {outcome}", rerun_subject(flow))
}

/// The button that deletes a merged pull request's head branch, and the title and confirm button
/// of its challenge.
pub const DELETE_BRANCH: &str = "Delete Branch";

/// The line beside the delete branch button.
#[must_use]
pub fn delete_branch_line(branch: &str) -> String {
    format!(
        "Deletes \u{201c}{branch}\u{201d} on GitHub for everyone. rc-git first writes a backup \
         ref in this repository that keeps its commit, and you type the branch's name to confirm."
    )
}

/// Why a merged pull request's head branch `branch` (as the read shows it) has no delete button:
/// it is in another repository.
#[must_use]
pub fn branch_in_fork(branch: &str) -> String {
    format!(
        "\u{201c}{branch}\u{201d} is in another repository than this one, so rc-git does not \
         delete it."
    )
}

/// Why: GitHub does not report that the account may delete it.
#[must_use]
pub fn branch_no_right(branch: &str) -> String {
    format!("GitHub does not report that your account may delete \u{201c}{branch}\u{201d}.")
}

/// Why: it is a protected branch.
#[must_use]
pub fn branch_protected(branch: &str) -> String {
    format!(
        "\u{201c}{branch}\u{201d} is a protected branch; rc-git never deletes it. Use git directly \
         if you are sure."
    )
}

/// Why: its name holds characters a confirmation shows as escapes, so it cannot be typed.
#[must_use]
pub fn branch_untypeable(branch: &str) -> String {
    format!(
        "\u{201c}{branch}\u{201d} holds characters rc-git shows as escapes, or is too long to \
         show, so its name cannot be typed to confirm. Delete it on GitHub's page or with git."
    )
}

/// The branch a delete acts on, as its plan named it; `None` before the plan landed.
fn planned_branch(flow: &DeleteBranchFlow) -> Option<&str> {
    flow.planned_as().map(|plan| plan.branch.as_str())
}

/// What the tab says for a head branch delete; `None` while the challenge is up.
#[must_use]
pub fn delete_branch_note(flow: &DeleteBranchFlow) -> Option<String> {
    let number = flow.number();
    let branch = planned_branch(flow).map_or_else(
        || "its head branch".to_owned(),
        |branch| format!("\u{201c}{branch}\u{201d}"),
    );
    let note = match flow.phase() {
        Phase::Planning => {
            format!("Reading #{number} and its head branch again before deleting it…")
        }
        Phase::Confirming => return None,
        Phase::Sending {
            sent: false,
            cancelling: false,
        } => format!(
            "Writing the backup ref, then deleting {branch} on GitHub… Cancel stops it until the \
             request is sent."
        ),
        Phase::Sending {
            sent: false,
            cancelling: true,
        } => format!("Stopping the delete of {branch}…"),
        Phase::Sending { sent: true, .. } => format!(
            "The request to delete {branch} was sent; it cannot be stopped now. Waiting for \
             GitHub's answer…"
        ),
        Phase::Done(Ended::NotSent(stop)) => stop_message(stop),
        Phase::Done(Ended::Sent(report)) => branch_report_note(&flow.ask().remote, report),
    };
    Some(note)
}

/// How to bring the branch of `report` back from its backup ref, through `remote`: a command the
/// user can paste, every word quoted for the shell.
#[must_use]
pub fn branch_restore(remote: &str, report: &BranchDeleteReport) -> String {
    let refspec = format!("{}:refs/heads/{}", report.backup_ref, report.branch);
    format!(
        "To bring the branch back, push the backup ref to GitHub under its name: git push {} {}",
        crate::shell_word::quote(remote),
        crate::shell_word::quote(&refspec)
    )
}

/// What a delete whose request left says: what GitHub did, the backup ref that keeps the commit,
/// how to restore the branch from it, and a local branch of the same name left alone.
fn branch_report_note(remote: &str, report: &BranchDeleteReport) -> String {
    const LOOK: &str = "Look at the branch on GitHub before trying again.";
    let branch = format!("\u{201c}{}\u{201d}", report.branch);
    let commit = short_hash(&report.head_oid);
    let kept = format!(
        "Its commit {commit} stays in this repository under the backup ref {}.",
        report.backup_ref
    );
    let restore = branch_restore(remote, report);
    let main = match &report.outcome {
        BranchDeleteOutcome::Deleted { settled } => {
            let how = match settled {
                Settled::Answer => "",
                Settled::Reread => {
                    " GitHub's answer was lost; reading it again shows the branch gone."
                }
            };
            format!("Deleted {branch} on GitHub.{how} {kept} {restore}")
        }
        BranchDeleteOutcome::Refused { reason } => format!(
            "GitHub refused to delete {branch}: {reason} Nothing was deleted. The backup ref {} \
             written first stays; delete it with git when you no longer need it.",
            report.backup_ref
        ),
        BranchDeleteOutcome::NotDeleted { problem } => format!(
            "{branch} was not deleted: GitHub did not answer ({problem}), and reading it again \
             shows it still on GitHub at {commit}. {LOOK} The backup ref {} written first stays.",
            report.backup_ref
        ),
        BranchDeleteOutcome::Unknown { problem, after } => format!(
            "rc-git could not tell whether {branch} was deleted: GitHub did not answer \
             ({problem}), and reading it again shows {}. {LOOK} {kept} If the branch is gone and \
             you need it: {restore}",
            branch_after_phrase(after)
        ),
    };
    if !report.local_branch {
        return main;
    }
    format!(
        "{main} This repository still has a local branch named {branch}; rc-git left it alone. \
         Delete it from the sidebar if you no longer need it."
    )
}

/// What the read after a lost delete answer showed.
fn branch_after_phrase(after: &BranchAfter) -> String {
    match after {
        BranchAfter::Changed { now: Some(now) } => format!(
            "a branch of that name at another commit, {}, or not the one you confirmed",
            short_hash(now)
        ),
        BranchAfter::Changed { now: None } => {
            "a branch of that name that is not the one you confirmed".to_owned()
        }
        BranchAfter::Unread { reason } => format!("nothing: that read failed too ({reason})"),
    }
}

/// `delete head branch “x” #7 of owner/name@host head <oid> backup <ref>`: the branch and the
/// backup slot the plan's read names.
fn branch_subject(flow: &DeleteBranchFlow) -> String {
    let (branch, backup) = flow.planned_as().map_or_else(
        || ("[not read]".to_owned(), "[not read]".to_owned()),
        |plan| {
            (
                format!("\u{201c}{}\u{201d}", plan.branch),
                plan.backup_ref.clone(),
            )
        },
    );
    format!(
        "delete head branch {branch} #{} of {} head {} backup {backup}",
        flow.number(),
        flow.repository(),
        flow.stamp().head_oid
    )
}

/// The app log's line for `flow`'s head branch delete, about to be sent. No token, no server
/// text but the guard's scrubbed messages and a branch name a challenge shows unchanged.
#[must_use]
pub fn branch_attempt_log(flow: &DeleteBranchFlow) -> String {
    format!("{}: sending", branch_subject(flow))
}

/// The app log's line for how `flow`'s head branch delete ended, naming the backup ref written.
#[must_use]
pub fn branch_outcome_log(flow: &DeleteBranchFlow, end: &DeleteBranchEnd) -> String {
    let outcome = match end {
        Ended::NotSent(stop) => format!("not sent: {}", stop_message(stop)),
        Ended::Sent(report) => {
            let what = match &report.outcome {
                BranchDeleteOutcome::Deleted {
                    settled: Settled::Answer,
                } => "deleted (answer)".to_owned(),
                BranchDeleteOutcome::Deleted {
                    settled: Settled::Reread,
                } => "deleted (read after a lost answer)".to_owned(),
                BranchDeleteOutcome::Refused { reason } => format!("refused: {reason}"),
                BranchDeleteOutcome::NotDeleted { problem } => {
                    format!("not deleted (read after a lost answer): {problem}")
                }
                BranchDeleteOutcome::Unknown { problem, after } => {
                    format!("unknown: {problem}; after: {after:?}")
                }
            };
            let local = if report.local_branch {
                "; local branch kept"
            } else {
                ""
            };
            format!("{what}; backup ref {} kept{local}", report.backup_ref)
        }
    };
    format!("{}: {outcome}", branch_subject(flow))
}

/// The check row's button that reads its job's log.
pub const VIEW_LOG: &str = "View Log";

/// The log panel's button that stops a read.
pub const STOP_LOG: &str = "Stop";

/// The log panel's button that closes it.
pub const CLOSE_LOG: &str = "Close Log";

/// The log panel's heading: `Log of “name”`.
#[must_use]
pub fn log_title(log: &LogState) -> String {
    format!("Log of \u{201c}{}\u{201d}", log.check().name)
}

/// What the log panel says while it reads or when it shows no log; `None` once the log shows.
#[must_use]
pub fn log_note(log: &LogState, now: u64) -> Option<String> {
    let name = &log.check().name;
    let note = match log.phase() {
        LogPhase::Matching => format!(
            "Checking that \u{201c}{name}\u{201d} ran on the head commit {}…",
            short_hash(&log.head())
        ),
        LogPhase::Downloading => format!("Downloading the log of \u{201c}{name}\u{201d}…"),
        LogPhase::Shown(_) => return None,
        LogPhase::NotMatched(why) => not_matched_note(why),
        LogPhase::NotFinished => not_finished_note(name),
        LogPhase::Failed(error) => log_problem(error, now),
    };
    Some(note)
}

/// What a log read that showed no log says, for the check named `name`, at `now`.
#[must_use]
pub fn log_stop_note(stop: &LogStop, name: &str, now: u64) -> String {
    match stop {
        LogStop::NotMatched(why) => not_matched_note(why),
        LogStop::NotFinished => not_finished_note(name),
        LogStop::Read(error) => log_problem(error, now),
    }
}

fn not_matched_note(why: &str) -> String {
    format!(
        "This check's log could not be matched to the pull request's head commit, so RCGit shows \
         none: {why}."
    )
}

fn not_finished_note(name: &str) -> String {
    format!("\u{201c}{name}\u{201d} has not finished; its log can be read once it has.")
}

/// What a failed log read says, at `now` (seconds since the Unix epoch).
#[must_use]
pub fn log_problem(error: &ReadError, now: u64) -> String {
    match error {
        ReadError::SignedOut { account } => format!(
            "Couldn't read the log: {account} is signed out, or refused its token. Sign in again \
             in Settings > GitHub Accounts."
        ),
        ReadError::RateLimited { reset: Some(reset) } if *reset > now => format!(
            "Couldn't read the log: the host's rate limit is spent for about {} more minutes.",
            (reset - now).div_ceil(60)
        ),
        ReadError::RateLimited { .. } => {
            "Couldn't read the log: the host's rate limit is spent.".to_owned()
        }
        ReadError::Cancelled => "Couldn't read the log: the read was stopped.".to_owned(),
        ReadError::Failed(cause) => format!("Couldn't read the log: {cause}"),
    }
}

/// What a log leaves out, one sentence each: the part not downloaded (`cut`), the lines
/// downloaded but not shown (`left_out`), the lines cut short (`shortened`). Empty for a whole log
/// shown whole.
#[must_use]
pub fn log_cut_notes(cut: LogCut, left_out: usize, shortened: usize) -> Vec<String> {
    let mut notes = Vec::new();
    let bytes = |kept: usize| format_bytes(u64::try_from(kept).unwrap_or(u64::MAX));
    match cut {
        LogCut::Whole => {}
        LogCut::Tail { kept } => notes.push(format!(
            "Showing the last {} of the log; the earlier part was not downloaded.",
            bytes(kept)
        )),
        LogCut::Head { kept } => notes.push(format!(
            "Showing the first {} of the log; the rest was not downloaded.",
            bytes(kept)
        )),
    }
    let count = |n: usize| grouped(u64::try_from(n).unwrap_or(u64::MAX));
    if left_out > 0 {
        let which = match cut {
            LogCut::Head { .. } => "last",
            LogCut::Whole | LogCut::Tail { .. } => "first",
        };
        notes.push(format!(
            "The {which} {} lines downloaded are not shown.",
            count(left_out)
        ));
    }
    if shortened > 0 {
        notes.push(format!(
            "{} long lines are cut at {} characters, ending in \u{2026}.",
            count(shortened),
            count(LOG_LINE_CHARS)
        ));
    }
    notes
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
