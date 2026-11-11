//! What the History pane says and when it pages (`HistoryViewController.swift`): the list's
//! placeholder, the scope and error bars, the search progress line, the commit detail header's
//! lines and the changed-file count. The pane only renders these.

use rc_git_core::bstr::ByteSlice;
use rc_git_core::{CommitDetail, CommitSummary, Oid, TreeChange};

use crate::dates::format_date_time;

/// How many rows before the end of the loaded list the next page is asked for: about the
/// 600 points of the Swift pane's prefetch margin at its row height.
pub const PREFETCH_ROWS: usize = 30;

/// Whether the list should ask for the next page now that rows up to `visible_end` are on
/// screen: the end is within [`PREFETCH_ROWS`] and the list is not `done` (complete, or stopped
/// on a failure). An empty list asks at once.
#[must_use]
pub fn wants_next_page(visible_end: usize, loaded: usize, done: bool) -> bool {
    !done && visible_end + PREFETCH_ROWS >= loaded
}

/// Where the commit list's rows come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSource {
    /// The anchor's history, `scoped` to one path or not, `complete` once its walk ended.
    History {
        /// Whether the walk reached the end.
        complete: bool,
        /// Whether the history is scoped to one path.
        scoped: bool,
    },
    /// A search's matches, `walking` while its walk runs.
    Search {
        /// Whether the search walk is running.
        walking: bool,
    },
}

/// What the commit list says while it has no rows, or `None` while it should say nothing: rows
/// are on screen, more are loading, or a failure (`failed`) stands in their place.
#[must_use]
pub fn list_placeholder(rows: usize, failed: bool, source: ListSource) -> Option<&'static str> {
    if rows > 0 || failed {
        return None;
    }
    match source {
        ListSource::Search { walking: true }
        | ListSource::History {
            complete: false, ..
        } => None,
        ListSource::Search { walking: false } => Some("No matching commits"),
        ListSource::History { scoped: true, .. } => Some("No commits touch this path"),
        ListSource::History { scoped: false, .. } => Some("No commits"),
    }
}

/// The scope bar's label for a history scoped to `path`.
#[must_use]
pub fn scope_label(path: &str) -> String {
    format!("History of {path}")
}

/// The error bar's label for a failed history page.
#[must_use]
pub fn history_error_label(error: &str) -> String {
    format!("Could not load history: {error}")
}

/// The search progress line: the failure once a chunk failed, a running count while the walk
/// goes on, the totals once it stops.
#[must_use]
pub fn search_progress(
    matches: usize,
    scanned: usize,
    searching: bool,
    error: Option<&str>,
) -> String {
    if let Some(error) = error {
        return format!("Search stopped: {error}");
    }
    if searching {
        return format!("Searched {scanned} commits\u{2026}");
    }
    format!("{matches} matches in {scanned} commits")
}

/// Hex digits a short commit hash shows, as in the Swift app's `Oid.short`.
pub const SHORT_HASH_LENGTH: usize = 7;

/// The short hash the list, the detail header and the parent links show.
#[must_use]
pub fn short_hash(oid: &Oid) -> String {
    let mut hex = oid.to_hex();
    hex.truncate(SHORT_HASH_LENGTH);
    hex
}

/// The detail header's title: the short hash, two spaces, the commit title.
#[must_use]
pub fn detail_title(commit: &CommitSummary) -> String {
    format!("{}  {}", short_hash(&commit.oid), commit.title)
}

/// One identity line of the detail header: `Author: Name <email> · Nov 11, 2026 at 3:04 PM`,
/// the time in a zone `offset` seconds east of UTC.
#[must_use]
pub fn identity_line(role: &str, name: &str, email: &str, time: i64, offset: i32) -> String {
    format!(
        "{role}: {name} <{email}> \u{b7} {}",
        format_date_time(time, offset)
    )
}

/// Whether the detail header shows a committer line: when any of name, email or time differs
/// from the author's.
#[must_use]
pub fn committer_differs(detail: &CommitDetail) -> bool {
    detail.committer_name != detail.summary.author_name
        || detail.committer_email != detail.summary.author_email
        || detail.committer_time != detail.summary.author_time
}

/// The label before the parent links: `Parent:` or `Parents:`; `None` for a root commit.
#[must_use]
pub fn parents_label(count: usize) -> Option<&'static str> {
    match count {
        0 => None,
        1 => Some("Parent:"),
        _ => Some("Parents:"),
    }
}

/// The changed-file count over the file list.
#[must_use]
pub fn changes_count(count: usize) -> String {
    match count {
        0 => "No files changed".to_owned(),
        1 => "1 file changed".to_owned(),
        _ => format!("{count} files changed"),
    }
}

/// The path over the diff, with a rename's source first: `old → new`.
#[must_use]
pub fn change_path_label(change: &TreeChange) -> String {
    let path = change.path.to_str_lossy();
    match &change.source_path {
        Some(source) => format!("{} \u{2192} {path}", source.to_str_lossy()),
        None => path.into_owned(),
    }
}

/// The blame window's Lines column: `12` for one line, `12–15` for a run.
#[must_use]
pub fn blame_lines(start_line: u32, line_count: u32) -> String {
    if line_count <= 1 {
        return start_line.to_string();
    }
    let last = start_line + line_count - 1;
    format!("{start_line}\u{2013}{last}")
}

#[cfg(test)]
mod tests {
    use rc_git_core::{ChangeKind, EntryKind};

    use super::*;

    fn oid(seed: u8) -> Oid {
        Oid::from_bytes(&[seed; 20]).unwrap()
    }

    fn summary() -> CommitSummary {
        CommitSummary {
            oid: oid(0xab),
            parents: vec![oid(1)],
            author_name: "Ann".to_owned(),
            author_email: "ann@example.com".to_owned(),
            author_time: 100,
            author_offset: 0,
            committer_time: 100,
            title: "Fix the thing".to_owned(),
        }
    }

    fn detail() -> CommitDetail {
        CommitDetail {
            summary: summary(),
            body: String::new(),
            committer_name: "Ann".to_owned(),
            committer_email: "ann@example.com".to_owned(),
            committer_time: 100,
            committer_offset: 0,
            tree: oid(2),
        }
    }

    #[test]
    fn the_next_page_is_asked_for_near_the_end_only() {
        assert!(wants_next_page(0, 0, false), "an empty list asks at once");
        assert!(wants_next_page(70, 100, false), "exactly the margin away");
        assert!(
            !wants_next_page(69, 100, false),
            "one row short of the margin"
        );
        assert!(!wants_next_page(100, 100, true), "nothing after the end");
    }

    #[test]
    fn the_placeholder_waits_for_the_walk_and_steps_aside_for_a_failure() {
        let history = |complete, scoped| ListSource::History { complete, scoped };
        let search = |walking| ListSource::Search { walking };
        assert_eq!(
            list_placeholder(0, false, history(true, false)),
            Some("No commits")
        );
        assert_eq!(
            list_placeholder(0, false, history(true, true)),
            Some("No commits touch this path")
        );
        assert_eq!(list_placeholder(0, false, history(false, false)), None);
        assert_eq!(list_placeholder(0, true, history(true, false)), None);
        assert_eq!(list_placeholder(1, false, history(true, false)), None);
        assert_eq!(
            list_placeholder(0, false, search(false)),
            Some("No matching commits")
        );
        assert_eq!(list_placeholder(0, false, search(true)), None);
        assert_eq!(list_placeholder(0, true, search(false)), None);
        assert_eq!(list_placeholder(2, false, search(false)), None);
    }

    #[test]
    fn the_search_line_says_what_the_walk_did() {
        assert_eq!(
            search_progress(3, 500, true, None),
            "Searched 500 commits\u{2026}"
        );
        assert_eq!(
            search_progress(3, 900, false, None),
            "3 matches in 900 commits"
        );
        assert_eq!(
            search_progress(3, 900, false, Some("bad pack")),
            "Search stopped: bad pack"
        );
    }

    #[test]
    fn the_header_lines_read_like_the_swift_pane() {
        let commit = summary();
        assert_eq!(detail_title(&commit), "abababa  Fix the thing");
        assert_eq!(
            identity_line("Author", "Ann", "ann@example.com", 0, 0),
            "Author: Ann <ann@example.com> \u{b7} Jan 1, 1970 at 12:00 AM"
        );
        assert_eq!(parents_label(0), None);
        assert_eq!(parents_label(1), Some("Parent:"));
        assert_eq!(parents_label(2), Some("Parents:"));
        assert_eq!(changes_count(0), "No files changed");
        assert_eq!(changes_count(1), "1 file changed");
        assert_eq!(changes_count(7), "7 files changed");
        assert_eq!(scope_label("src/a.rs"), "History of src/a.rs");
        assert_eq!(history_error_label("boom"), "Could not load history: boom");
    }

    #[test]
    fn the_committer_line_shows_when_any_part_differs() {
        assert!(!committer_differs(&detail()));
        let mut moved = detail();
        moved.committer_time = 101;
        assert!(committer_differs(&moved));
        let mut renamed = detail();
        renamed.committer_name = "Bo".to_owned();
        assert!(committer_differs(&renamed));
        let mut mailed = detail();
        mailed.committer_email = "bo@example.com".to_owned();
        assert!(committer_differs(&mailed));
    }

    #[test]
    fn a_rename_names_its_source_first() {
        let mut change = TreeChange {
            kind: ChangeKind::Renamed,
            path: "b.rs".into(),
            source_path: Some("a.rs".into()),
            old: Some(oid(1)),
            new: Some(oid(2)),
            entry: EntryKind::Blob,
            old_entry: Some(EntryKind::Blob),
        };
        assert_eq!(change_path_label(&change), "a.rs \u{2192} b.rs");
        change.source_path = None;
        assert_eq!(change_path_label(&change), "b.rs");
    }

    #[test]
    fn blame_lines_name_a_run_by_its_ends() {
        assert_eq!(blame_lines(12, 1), "12");
        assert_eq!(blame_lines(12, 4), "12\u{2013}15");
        assert_eq!(blame_lines(12, 0), "12");
    }
}
