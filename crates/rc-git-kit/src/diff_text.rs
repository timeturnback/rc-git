//! Port of the text parts of `DiffModels.swift` and `WorktreeModels.swift`: the one-letter
//! status labels, the line a diff pane shows in place of hunks, and which hunk owns a row of
//! the Changes diff pane.

use rc_git_core::{BlobDiff, ChangeKind, DiffHunk, WorktreeStatus};

use crate::text_document::{format_bytes, format_over_limit};

/// One-letter label for a row badge, as `git status` prints it; `None` when the path is
/// unchanged.
#[must_use]
pub fn worktree_letter(status: WorktreeStatus) -> Option<&'static str> {
    match status {
        WorktreeStatus::Unchanged => None,
        WorktreeStatus::Modified => Some("M"),
        WorktreeStatus::Untracked => Some("U"),
        WorktreeStatus::Ignored => Some("I"),
        WorktreeStatus::Deleted => Some("D"),
    }
}

/// One-letter label for a changed file between two trees, as `git status` prints it.
#[must_use]
pub fn change_letter(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Deleted => "D",
        ChangeKind::Modified => "M",
        ChangeKind::Renamed => "R",
    }
}

/// The line a diff pane shows in place of hunks, or `None` when there are hunks to show.
#[must_use]
pub fn placeholder(diff: &BlobDiff, path: &str) -> Option<String> {
    match diff {
        BlobDiff::Text { hunks } => hunks.is_empty().then(|| format!("{path}: no line changes")),
        BlobDiff::Binary { old_size, new_size } => Some(format!(
            "{path} is binary ({} \u{2192} {})",
            format_bytes(*old_size),
            format_bytes(*new_size)
        )),
        BlobDiff::TooLarge { size, limit } => Some(too_large_placeholder(path, *size, *limit)),
    }
}

/// The line a pane shows for a file over its size limit.
#[must_use]
pub fn too_large_placeholder(path: &str, size: u64, limit: u64) -> String {
    let (size, limit) = format_over_limit(size, limit);
    format!("{path} is {size}, over the {limit} limit")
}

/// The hunk that owns `row` of a diff pane that shows each hunk as its header row followed by
/// one row per line, so an action on any row of a hunk acts on that hunk. `None` for a row
/// past the pane's end.
#[must_use]
pub fn owning_hunk(row: usize, hunks: &[DiffHunk]) -> Option<&DiffHunk> {
    let mut end = 0;
    for hunk in hunks {
        end += 1 + hunk.lines.len();
        if row < end {
            return Some(hunk);
        }
    }
    None
}
