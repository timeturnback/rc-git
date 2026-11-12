//! What the Changes views decide from the model's state (`ChangesViewController.swift` and
//! the window controller's commit and stash steps), as values the views only apply: the scan
//! truncation note, the diff pane's placeholder line, a stash row's label, which way a tick
//! box click goes, whether the stash group shows, and the steps before a commit or a stash.

use rc_git_core::StashEntry;

use crate::changes::{ChangesState, RowTick, StashListState, TickState, WorktreeDiffState};
use crate::commit_identity::CommitAuthor;
use crate::diff_text;

/// What the diff pane says before a file is picked.
pub const SELECT_A_FILE: &str = "Select a file";

/// The alert title for a Commit with an empty message.
pub const NO_MESSAGE_TITLE: &str = "No commit message";

/// The alert text for a Commit with an empty message.
pub const NO_MESSAGE_TEXT: &str = "Enter a commit message first.";

/// The note under the change list when the scan stopped at its cap, `None` otherwise.
#[must_use]
pub fn truncation_note(state: &ChangesState) -> Option<String> {
    match state {
        ChangesState::Loaded(report) if report.truncated => Some(format!(
            "Scan stopped after {} entries; the list is incomplete.",
            report.scanned
        )),
        ChangesState::Loaded(_)
        | ChangesState::Idle
        | ChangesState::Loading
        | ChangesState::Failed(_) => None,
    }
}

/// The line the diff pane shows in place of hunks, or `None` when it shows hunks.
#[must_use]
pub fn diff_placeholder(state: &WorktreeDiffState) -> Option<String> {
    match state {
        WorktreeDiffState::Idle => Some(SELECT_A_FILE.to_owned()),
        WorktreeDiffState::Loading(path) => Some(format!("Loading {path}\u{2026}")),
        WorktreeDiffState::Failed(_, message) => Some(message.clone()),
        WorktreeDiffState::Loaded(path, diff) => diff_text::placeholder(diff, path),
    }
}

/// The path the diff pane shows or loads, `None` before a file is picked.
#[must_use]
pub fn diff_path(state: &WorktreeDiffState) -> Option<&str> {
    match state {
        WorktreeDiffState::Idle => None,
        WorktreeDiffState::Loading(path)
        | WorktreeDiffState::Loaded(path, _)
        | WorktreeDiffState::Failed(path, _) => Some(path),
    }
}

/// A stash row's label: its position, then its message.
#[must_use]
pub fn stash_label(entry: &StashEntry) -> String {
    format!("stash@{{{}}}  {}", entry.index, entry.message)
}

/// What a click on a tick box asks for. A mixed box (a folder with some files ticked, a file
/// with some hunks ticked) has one sensible answer: tick what is not ticked yet.
#[must_use]
pub fn tick_target(tick: RowTick) -> bool {
    tick.state != TickState::On
}

/// Whether the stash group shows: a repository with no stash hides the heading, list and
/// buttons together, but a failed read shows its error in place of the list.
#[must_use]
pub fn shows_stash_group(has_stashes: bool, state: &StashListState) -> bool {
    has_stashes || *state != StashListState::Loaded
}

/// What a Commit or Stash click does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommitStep {
    /// Nothing is selected: nothing happens.
    Nothing,
    /// The message is empty: say so.
    NeedsMessage,
    /// Nobody is set to record: ask for an identity first, then run.
    AskIdentity,
    /// Plan the action and follow the decision.
    Run,
}

/// The step before a commit. `author` is `None` while the first read has not landed, which
/// runs and lets the guard decide; `prompted` says an identity was just asked for.
#[must_use]
pub fn commit_step(
    has_selection: bool,
    message: &str,
    author: Option<&CommitAuthor>,
    prompted: bool,
) -> CommitStep {
    if !has_selection {
        return CommitStep::Nothing;
    }
    if message.trim().is_empty() {
        return CommitStep::NeedsMessage;
    }
    identity_step(author, prompted)
}

/// The step before a stash, which takes an empty message as git's default.
#[must_use]
pub fn stash_step(has_paths: bool, author: Option<&CommitAuthor>, prompted: bool) -> CommitStep {
    if !has_paths {
        return CommitStep::Nothing;
    }
    identity_step(author, prompted)
}

fn identity_step(author: Option<&CommitAuthor>, prompted: bool) -> CommitStep {
    if !prompted && author == Some(&CommitAuthor::Unset) {
        CommitStep::AskIdentity
    } else {
        CommitStep::Run
    }
}

/// The stash message the commit box hands on: its text trimmed, `None` when nothing is left,
/// which gives git's `WIP on <branch>` default.
#[must_use]
pub fn stash_message(raw: &str) -> Option<String> {
    let message = raw.trim();
    (!message.is_empty()).then(|| message.to_owned())
}

#[cfg(test)]
mod tests {
    use rc_git_core::{BlobDiff, Oid, StatusReport};

    use super::*;
    use crate::commit_identity::CommitIdentity;

    fn report(truncated: bool) -> StatusReport {
        StatusReport {
            scanned: 7,
            truncated,
            ..StatusReport::default()
        }
    }

    #[test]
    fn only_a_truncated_report_carries_a_note() {
        assert_eq!(
            truncation_note(&ChangesState::Loaded(report(true))).as_deref(),
            Some("Scan stopped after 7 entries; the list is incomplete.")
        );
        assert_eq!(truncation_note(&ChangesState::Loaded(report(false))), None);
        assert_eq!(truncation_note(&ChangesState::Loading), None);
        assert_eq!(truncation_note(&ChangesState::Failed("x".into())), None);
    }

    #[test]
    fn the_diff_pane_says_what_it_waits_for() {
        assert_eq!(
            diff_placeholder(&WorktreeDiffState::Idle).as_deref(),
            Some(SELECT_A_FILE)
        );
        assert_eq!(
            diff_placeholder(&WorktreeDiffState::Loading("a.rs".into())).as_deref(),
            Some("Loading a.rs\u{2026}")
        );
        assert_eq!(
            diff_placeholder(&WorktreeDiffState::Failed("a.rs".into(), "gone".into())).as_deref(),
            Some("gone")
        );
        let empty = BlobDiff::Text { hunks: Vec::new() };
        assert_eq!(
            diff_placeholder(&WorktreeDiffState::Loaded("a.rs".into(), empty)).as_deref(),
            Some("a.rs: no line changes")
        );
        assert_eq!(diff_path(&WorktreeDiffState::Idle), None);
        assert_eq!(
            diff_path(&WorktreeDiffState::Loading("a.rs".into())),
            Some("a.rs")
        );
    }

    #[test]
    fn a_stash_row_names_its_position() {
        let entry = StashEntry {
            index: 2,
            oid: Oid::from_bytes(&[1; 20]).unwrap(),
            message: "WIP on main".into(),
        };
        assert_eq!(stash_label(&entry), "stash@{2}  WIP on main");
    }

    #[test]
    fn a_click_ticks_unless_everything_is_ticked() {
        let tick = |state| RowTick {
            state,
            is_enabled: true,
        };
        assert!(tick_target(tick(TickState::Off)));
        assert!(tick_target(tick(TickState::Mixed)));
        assert!(!tick_target(tick(TickState::On)));
    }

    #[test]
    fn the_stash_group_hides_only_when_empty_and_current() {
        assert!(!shows_stash_group(false, &StashListState::Loaded));
        assert!(shows_stash_group(true, &StashListState::Loaded));
        assert!(shows_stash_group(
            false,
            &StashListState::Failed("x".into())
        ));
    }

    #[test]
    fn a_commit_needs_a_selection_then_a_message_then_an_author() {
        let unset = CommitAuthor::Unset;
        let saved = CommitAuthor::Saved(CommitIdentity {
            name: "A".into(),
            email: "a@b".into(),
        });
        assert_eq!(
            commit_step(false, "msg", Some(&saved), false),
            CommitStep::Nothing
        );
        assert_eq!(
            commit_step(true, " \n ", Some(&saved), false),
            CommitStep::NeedsMessage
        );
        assert_eq!(
            commit_step(true, "msg", Some(&unset), false),
            CommitStep::AskIdentity
        );
        assert_eq!(
            commit_step(true, "msg", Some(&unset), true),
            CommitStep::Run
        );
        assert_eq!(commit_step(true, "msg", None, false), CommitStep::Run);
        assert_eq!(
            commit_step(true, "msg", Some(&saved), false),
            CommitStep::Run
        );
    }

    #[test]
    fn a_stash_takes_no_message_but_still_needs_an_author() {
        let unset = CommitAuthor::Unset;
        assert_eq!(stash_step(false, None, false), CommitStep::Nothing);
        assert_eq!(
            stash_step(true, Some(&unset), false),
            CommitStep::AskIdentity
        );
        assert_eq!(stash_step(true, Some(&unset), true), CommitStep::Run);
        assert_eq!(stash_message("  "), None);
        assert_eq!(stash_message(" wip \n").as_deref(), Some("wip"));
    }
}
