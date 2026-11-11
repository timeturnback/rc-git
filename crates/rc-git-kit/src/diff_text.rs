//! Port of the text parts of `DiffModels.swift` and `WorktreeModels.swift`: the one-letter
//! status labels, the line a diff pane shows in place of hunks, and which hunk owns a row of
//! the Changes diff pane.

use rc_git_core::{BlobDiff, ChangeKind, DiffHunk, WorktreeStatus};

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
            byte_format::string(*old_size),
            byte_format::string(*new_size)
        )),
        BlobDiff::TooLarge { size, limit } => Some(too_large_placeholder(path, *size, *limit)),
    }
}

/// The line a pane shows for a file over its size limit.
#[must_use]
pub fn too_large_placeholder(path: &str, size: u64, limit: u64) -> String {
    let (size, limit) = byte_format::over_limit(size, limit);
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

// Dedupe with text_document byte format at merge
mod byte_format {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];

    /// A byte count in decimal units, one decimal under 10. Picks the unit and the digit count
    /// from the value as it will print, so a value that rounds up to 1000 carries to the next
    /// unit and one that rounds up to 10 drops its decimal.
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display string: sizes past 2^53 bytes lose digits nobody reads"
    )]
    pub(super) fn string(bytes: u64) -> String {
        let mut value = bytes as f64;
        let mut unit = 0;
        while value.round() >= 1000.0 && unit < UNITS.len() - 1 {
            value /= 1000.0;
            unit += 1;
        }
        if unit == 0 {
            return format!("{bytes} B");
        }
        let digits = usize::from((value * 10.0).round() / 10.0 < 10.0);
        format!("{value:.digits$} {}", UNITS[unit])
    }

    /// A size and the limit it is over, worded for "is <size>, over the <limit> limit". Falls
    /// back to exact byte counts when the short strings would read the same.
    pub(super) fn over_limit(size: u64, limit: u64) -> (String, String) {
        let short = (string(size), string(limit));
        if short.0 != short.1 {
            return short;
        }
        (
            format!("{} bytes", grouped(size)),
            format!("{}-byte", grouped(limit)),
        )
    }

    /// Digits in groups of three separated by commas, independent of the user's locale.
    fn grouped(value: u64) -> String {
        let digits = value.to_string();
        let mut out = String::new();
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                out.push(',');
            }
            out.push(digit);
        }
        out
    }
}
