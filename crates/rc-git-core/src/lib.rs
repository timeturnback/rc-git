//! Read-only access to a git repository's object database.
//!
//! Everything is keyed by [`Oid`]. Git objects are immutable, so any cache built on these
//! results stays valid until the repository's refs change. Nothing here writes to the
//! repository, the working tree, or the filesystem: `clippy.toml` at the workspace root rejects
//! the APIs that could, and `tests/read_only.rs` checks that `.git` is byte-identical after
//! every operation.

mod blame;
mod blob;
mod diff;
mod error;
mod history;
mod log;
mod oid;
mod refs;
mod repo;
mod stash;
mod tree;
mod walk;
mod worktree;

pub use blame::{Blame, BlameRange};
pub use blob::{BlobContent, DEFAULT_BLOB_LIMIT, GIT_BINARY_PROBE_BYTES, is_binary, line_starts};
pub use diff::{
    BlobDiff, ChangeKind, DEFAULT_DIFF_CONTEXT, DiffHunk, DiffLine, DiffLineKind, LineEnd,
    TreeChange,
};
pub use error::{BoxError, Error, ObjectKind, Result};
pub use gix::bstr;
pub use history::{CommitFilter, CommitPage, HistoryWalk};
pub use log::{CommitDetail, CommitSummary, LogCursor};
pub use oid::Oid;
pub use refs::{BrokenRef, Head, RefEntry, RefKind, RefListing};
pub use repo::{Repository, require_files_ref_storage, require_work_tree_at};
pub use stash::{StashEntry, StashReflogLine, stash_reflog_lines};
pub use tree::{EntryKind, TreeEntry, sort_for_display};
pub use worktree::{
    StatusEntry, StatusReport, UnreadableEntry, WorktreeEntry, WorktreeFile, WorktreeFileList,
    WorktreeListing, WorktreeStatus,
};
