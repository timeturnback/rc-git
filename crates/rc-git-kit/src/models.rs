//! The parts of `Models.swift`, `WorktreeModels.swift` and `DiffModels.swift` no other module
//! carries: the refusal for a name that crossed with replacements, the tree pane's row, the
//! history diff and blame states, and which side of a change is a blob. The value types
//! themselves are core's; the text helpers live in [`crate::diff_text`] and
//! [`crate::ref_filter`].

use rc_git_core::{
    BlameRange, BlobDiff, CommitSummary, EntryKind, LineEnd, Oid, TreeChange, TreeEntry,
    WorktreeStatus,
};

/// The refusal for `action` (a verb phrase, such as "blame") on `path`, a path that is not
/// valid UTF-8: its repaired spelling would act on whatever has that name, possibly another
/// object.
#[must_use]
pub fn path_refusal(action: &str, path: &str) -> String {
    format!("cannot {action} '{path}': the path is not valid UTF-8")
}

/// The refusal for `action` (a verb phrase, such as "switch to") on branch `name`, a name
/// that is not valid UTF-8.
#[must_use]
pub fn branch_refusal(action: &str, name: &str) -> String {
    format!("cannot {action} '{name}': the branch name is not valid UTF-8")
}

/// One row of the tree pane: a tree or working-tree entry with its optional status overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseRow {
    /// The entry.
    pub entry: TreeEntry,
    /// The working-tree status; `None` when browsing a commit.
    pub status: Option<WorktreeStatus>,
    /// True for a file the compared tree has but disk lost: shown as deleted.
    pub missing: bool,
}

/// The blob before `change`: `None` for an addition, and for a gitlink, whose id names a
/// commit in another repository.
#[must_use]
pub fn old_blob(change: &TreeChange) -> Option<Oid> {
    match change.old_entry {
        Some(EntryKind::Submodule) => None,
        Some(_) | None => change.old,
    }
}

/// The blob after `change`: `None` for a deletion, and for a gitlink.
#[must_use]
pub fn new_blob(change: &TreeChange) -> Option<Oid> {
    match change.entry {
        EntryKind::Submodule => None,
        EntryKind::Tree | EntryKind::Blob | EntryKind::ExecutableBlob | EntryKind::Symlink => {
            change.new
        }
    }
}

/// The note drawn after a diff line whose terminator is not a plain `\n`, so a change in line
/// ending alone does not render as two identical lines. `None` for `\n`.
#[must_use]
pub fn line_end_marker(eol: LineEnd) -> Option<&'static str> {
    match eol {
        LineEnd::Lf => None,
        LineEnd::CrLf => Some(" \u{240D}CRLF"),
        LineEnd::None => Some(" \u{2205} no newline at end of file"),
    }
}

/// What the history detail's diff pane shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiffState {
    /// No change picked.
    Empty,
    /// The change's diff is loading.
    Loading(TreeChange),
    /// The change and its diff.
    Loaded(TreeChange, BlobDiff),
    /// A change with a gitlink on every side: its ids name commits in another repository, so
    /// there is no line diff.
    Submodule(TreeChange),
    /// The change and why its diff failed.
    Failed(TreeChange, String),
}

/// The line the diff pane shows for a submodule change, as git words it.
#[must_use]
pub fn submodule_summary(change: &TreeChange) -> String {
    let side = |oid: Option<Oid>| oid.map_or_else(|| "none".to_owned(), |oid| oid.to_hex());
    format!(
        "Subproject commit {} \u{2192} {}",
        side(change.old),
        side(change.new)
    )
}

/// One blame range together with the commit that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameSpan {
    /// The range.
    pub range: BlameRange,
    /// The commit that introduced it, or for a boundary range the commit the walk stopped at.
    pub commit: CommitSummary,
    /// For a boundary range, the floor the walk stopped at, in seconds since the Unix epoch:
    /// the lines are older than it. `None` when the range was traced to its commit.
    pub older_than: Option<i64>,
}

/// The blame the model last started or finished for the open file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlameState {
    /// No blame is showing: none was asked for, or the one running was dropped or cancelled.
    Empty,
    /// A walk of the file at `path` is running; `title` says so while the panel waits.
    Loading {
        /// The file being blamed.
        path: String,
        /// The panel's title.
        title: String,
    },
    /// Every range of the file at `path`, paired with its commit, and the panel's title.
    Loaded {
        /// The file blamed.
        path: String,
        /// Its ranges in line order.
        spans: Vec<BlameSpan>,
        /// The panel's title.
        title: String,
    },
    /// The blame failed; the message says why.
    Failed(String),
}
