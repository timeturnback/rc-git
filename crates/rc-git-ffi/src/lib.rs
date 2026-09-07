//! C ABI over `rc-git-core` for the Swift app. `docs/ffi.md` explains how to extend it.
//!
//! Rules every function follows:
//!
//! - A pointer returned by an `rcgit_*` function belongs to the caller until it is passed to the
//!   matching `rcgit_*_free`. Freeing NULL is a no-op.
//! - A `char *` is NUL-terminated. Names that are not UTF-8 pass through as raw bytes.
//! - A function that can fail takes `char **error`. On failure it returns NULL (or `RCGIT_ERROR`)
//!   and, when `error` is not NULL, stores a message to release with `rcgit_string_free`. On
//!   success `*error` is left untouched.
//! - An `RcGitRepo` may be used from any thread, including several at once.

use std::ffi::{CStr, CString, OsStr, c_char};
use std::fmt::Display;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use rc_git_core::{
    BlobContent, BlobDiff, ChangeKind, CommitDetail, CommitFilter, CommitPage, CommitSummary,
    DiffHunk, DiffLineKind, EntryKind, Oid, RefKind, Repository, TreeChange, TreeEntry,
    WorktreeStatus,
};
use rc_git_guard::actions::{
    ActionError, create_branch, create_tag, delete_branch, delete_tag, plan_branch_delete,
    plan_tag_delete,
};
use rc_git_guard::{Decision, Policy, Risk};
use rc_git_highlight::{Class, Language};

/// Status for functions that fill an out parameter: success.
pub const RCGIT_OK: i32 = 0;
/// Status for functions that fill an out parameter: failure, see `error`.
pub const RCGIT_ERROR: i32 = -1;

/// An object id: the first `len` bytes of `bytes` are the hash (20 for SHA-1).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcGitOid {
    /// Hash bytes, zero-padded.
    pub bytes: [u8; 32],
    /// Number of meaningful bytes.
    pub len: u8,
}

impl From<Oid> for RcGitOid {
    fn from(oid: Oid) -> Self {
        let raw = oid.as_bytes();
        let mut bytes = [0u8; 32];
        bytes[..raw.len()].copy_from_slice(raw);
        Self {
            bytes,
            len: u8::try_from(raw.len()).unwrap_or(0),
        }
    }
}

impl RcGitOid {
    fn to_oid(self) -> Option<Oid> {
        let len = usize::from(self.len);
        if len > self.bytes.len() {
            return None;
        }
        Oid::from_bytes(&self.bytes[..len])
    }
}

/// An open repository. Opaque: create with `rcgit_repo_open`, release with `rcgit_repo_free`.
pub struct RcGitRepo(Repository);

/// Where `HEAD` points.
#[repr(C)]
pub struct RcGitHead {
    /// Full branch name such as `refs/heads/main`, or NULL when detached.
    pub ref_name: *mut c_char,
    /// False on an unborn branch, in which case `target` is meaningless.
    pub has_target: bool,
    /// The commit `HEAD` resolves to.
    pub target: RcGitOid,
}

impl Drop for RcGitHead {
    fn drop(&mut self) {
        // SAFETY: `ref_name` was produced by `export_string` and is released exactly once here.
        unsafe { release_string(self.ref_name) };
    }
}

/// Namespace of a reference.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitRefKind {
    /// `refs/heads/*`
    LocalBranch = 0,
    /// `refs/remotes/*`
    RemoteBranch = 1,
    /// `refs/tags/*`
    Tag = 2,
    /// Anything else.
    Other = 3,
}

impl From<RefKind> for RcGitRefKind {
    fn from(kind: RefKind) -> Self {
        match kind {
            RefKind::LocalBranch => Self::LocalBranch,
            RefKind::RemoteBranch => Self::RemoteBranch,
            RefKind::Tag => Self::Tag,
            RefKind::Other => Self::Other,
        }
    }
}

/// A reference and the object it finally points to.
#[repr(C)]
pub struct RcGitRef {
    /// Full name, e.g. `refs/heads/main`.
    pub full_name: *mut c_char,
    /// Name without the namespace, e.g. `main`, `origin/main`, `v1.0`.
    pub short_name: *mut c_char,
    /// Namespace classification.
    pub kind: RcGitRefKind,
    /// Peeled target object.
    pub target: RcGitOid,
}

impl Drop for RcGitRef {
    fn drop(&mut self) {
        // SAFETY: both strings were produced by `export_string` and are released once here.
        unsafe {
            release_string(self.full_name);
            release_string(self.short_name);
        }
    }
}

/// A reference whose target could not be read.
#[repr(C)]
pub struct RcGitBrokenRef {
    /// Full name of the reference.
    pub full_name: *mut c_char,
    /// Why it could not be resolved.
    pub error: *mut c_char,
}

impl Drop for RcGitBrokenRef {
    fn drop(&mut self) {
        // SAFETY: both strings were produced by `export_string` and are released once here.
        unsafe {
            release_string(self.full_name);
            release_string(self.error);
        }
    }
}

/// Every reference in the repository.
#[repr(C)]
pub struct RcGitRefList {
    /// `len` resolved references.
    pub refs: *mut RcGitRef,
    /// Number of entries in `refs`.
    pub len: usize,
    /// `broken_len` references that failed to resolve; normally none.
    pub broken: *mut RcGitBrokenRef,
    /// Number of entries in `broken`.
    pub broken_len: usize,
}

impl Drop for RcGitRefList {
    fn drop(&mut self) {
        // SAFETY: both arrays were produced by `export_vec` with these lengths.
        unsafe {
            release_vec(self.refs, self.len);
            release_vec(self.broken, self.broken_len);
        }
    }
}

/// What a tree entry points at.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitEntryKind {
    /// A directory.
    Tree = 0,
    /// A regular file.
    Blob = 1,
    /// A file with the executable bit.
    ExecutableBlob = 2,
    /// A symbolic link; the blob holds the target path.
    Symlink = 3,
    /// A submodule; the id is a commit in another repository.
    Submodule = 4,
}

impl From<EntryKind> for RcGitEntryKind {
    fn from(kind: EntryKind) -> Self {
        match kind {
            EntryKind::Tree => Self::Tree,
            EntryKind::Blob => Self::Blob,
            EntryKind::ExecutableBlob => Self::ExecutableBlob,
            EntryKind::Symlink => Self::Symlink,
            EntryKind::Submodule => Self::Submodule,
        }
    }
}

/// One child of a tree.
#[repr(C)]
pub struct RcGitTreeEntry {
    /// File name as stored; may be non-UTF-8.
    pub name: *mut c_char,
    /// Id of the child object.
    pub oid: RcGitOid,
    /// What the child is.
    pub kind: RcGitEntryKind,
}

impl RcGitTreeEntry {
    fn export(entry: TreeEntry) -> Self {
        Self {
            name: export_string(Vec::from(entry.name)),
            oid: entry.oid.into(),
            kind: entry.kind.into(),
        }
    }
}

impl Drop for RcGitTreeEntry {
    fn drop(&mut self) {
        // SAFETY: `name` was produced by `export_string` and is released once here.
        unsafe { release_string(self.name) };
    }
}

/// Children of one tree, ordered for display: directories first, then names case-insensitively.
#[repr(C)]
pub struct RcGitTreeList {
    /// `len` entries.
    pub entries: *mut RcGitTreeEntry,
    /// Number of entries.
    pub len: usize,
}

impl Drop for RcGitTreeList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.entries, self.len) };
    }
}

/// What a blob read produced.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitBlobKind {
    /// `data` and `line_starts` are filled.
    Text = 0,
    /// Binary; only `size` is filled.
    Binary = 1,
    /// Over the limit; `size` and `limit` are filled, nothing was decoded.
    TooLarge = 2,
}

/// File contents.
#[repr(C)]
pub struct RcGitBlob {
    /// Which fields are meaningful.
    pub kind: RcGitBlobKind,
    /// Size of the blob in bytes.
    pub size: u64,
    /// The limit passed to `rcgit_blob_read`.
    pub limit: u64,
    /// `data_len` bytes of content, not NUL-terminated. Text only.
    pub data: *mut u8,
    /// Length of `data`.
    pub data_len: usize,
    /// `line_count` byte offsets where lines begin. Text only.
    pub line_starts: *mut u32,
    /// Length of `line_starts`.
    pub line_count: usize,
}

impl Drop for RcGitBlob {
    fn drop(&mut self) {
        // SAFETY: both arrays were produced by `export_vec` with these lengths.
        unsafe {
            release_vec(self.data, self.data_len);
            release_vec(self.line_starts, self.line_count);
        }
    }
}

/// The parts of a commit a list shows.
#[repr(C)]
pub struct RcGitCommit {
    /// The commit id.
    pub oid: RcGitOid,
    /// `parent_count` parent ids, first parent first.
    pub parents: *mut RcGitOid,
    /// Number of parents.
    pub parent_count: usize,
    /// Author name.
    pub author_name: *mut c_char,
    /// Author email.
    pub author_email: *mut c_char,
    /// Author time, seconds since the Unix epoch.
    pub author_time: i64,
    /// Author timezone, seconds east of UTC.
    pub author_offset: i32,
    /// First paragraph of the message.
    pub title: *mut c_char,
}

impl RcGitCommit {
    fn export(commit: CommitSummary) -> Self {
        let mut parents = Vec::with_capacity(commit.parents.len());
        for parent in commit.parents {
            parents.push(RcGitOid::from(parent));
        }
        let (parents, parent_count) = export_vec(parents);
        Self {
            oid: commit.oid.into(),
            parents,
            parent_count,
            author_name: export_string(commit.author_name),
            author_email: export_string(commit.author_email),
            author_time: commit.author_time,
            author_offset: commit.author_offset,
            title: export_string(commit.title),
        }
    }
}

impl Drop for RcGitCommit {
    fn drop(&mut self) {
        // SAFETY: every pointer was produced by `export_string`/`export_vec` and is released once.
        unsafe {
            release_vec(self.parents, self.parent_count);
            release_string(self.author_name);
            release_string(self.author_email);
            release_string(self.title);
        }
    }
}

/// Everything a commit detail view shows beyond the summary.
#[repr(C)]
pub struct RcGitCommitDetail {
    /// The list-view parts of the commit. Owned by this struct; never freed separately.
    pub summary: RcGitCommit,
    /// Message body after the title. Empty when the message is only a title.
    pub body: *mut c_char,
    /// Committer name.
    pub committer_name: *mut c_char,
    /// Committer email.
    pub committer_email: *mut c_char,
    /// Committer time, seconds since the Unix epoch.
    pub committer_time: i64,
    /// Committer timezone, seconds east of UTC.
    pub committer_offset: i32,
    /// The root tree of the commit.
    pub tree: RcGitOid,
}

impl RcGitCommitDetail {
    fn export(detail: CommitDetail) -> Self {
        Self {
            summary: RcGitCommit::export(detail.summary),
            body: export_string(detail.body),
            committer_name: export_string(detail.committer_name),
            committer_email: export_string(detail.committer_email),
            committer_time: detail.committer_time,
            committer_offset: detail.committer_offset,
            tree: detail.tree.into(),
        }
    }
}

impl Drop for RcGitCommitDetail {
    fn drop(&mut self) {
        // SAFETY: every pointer was produced by `export_string` and is released once; `summary`
        // releases its own pointers through its `Drop`.
        unsafe {
            release_string(self.body);
            release_string(self.committer_name);
            release_string(self.committer_email);
        }
    }
}

/// A page of commits.
#[repr(C)]
pub struct RcGitCommitList {
    /// `len` commits, newest first.
    pub commits: *mut RcGitCommit,
    /// Number of commits.
    pub len: usize,
}

impl Drop for RcGitCommitList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.commits, self.len) };
    }
}

/// One page of a filtered history walk.
#[repr(C)]
pub struct RcGitCommitPage {
    /// `len` matching commits, newest first.
    pub commits: *mut RcGitCommit,
    /// Number of matching commits.
    pub len: usize,
    /// How many commits the call examined past its `scan_skip`.
    pub scanned: usize,
    /// True when the walk reached the end of history within this call.
    pub complete: bool,
}

impl RcGitCommitPage {
    fn export(page: CommitPage) -> Self {
        let mut exported = Vec::with_capacity(page.commits.len());
        for commit in page.commits {
            exported.push(RcGitCommit::export(commit));
        }
        let (commits, len) = export_vec(exported);
        Self {
            commits,
            len,
            scanned: page.scanned,
            complete: page.complete,
        }
    }
}

impl Drop for RcGitCommitPage {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.commits, self.len) };
    }
}

/// What happened to one path between two trees.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitChangeKind {
    /// Exists only in the new tree.
    Added = 0,
    /// Exists only in the old tree.
    Deleted = 1,
    /// Exists in both trees with different content or mode.
    Modified = 2,
    /// Content moved from `source_path`.
    Renamed = 3,
    /// Content copied from `source_path`.
    Copied = 4,
}

impl From<ChangeKind> for RcGitChangeKind {
    fn from(kind: ChangeKind) -> Self {
        match kind {
            ChangeKind::Added => Self::Added,
            ChangeKind::Deleted => Self::Deleted,
            ChangeKind::Modified => Self::Modified,
            ChangeKind::Renamed => Self::Renamed,
            ChangeKind::Copied => Self::Copied,
        }
    }
}

/// One changed file between two trees.
#[repr(C)]
pub struct RcGitTreeChange {
    /// What happened.
    pub kind: RcGitChangeKind,
    /// Path in the new tree, or in the old tree for a deletion.
    pub path: *mut c_char,
    /// Where the content came from; NULL unless renamed or copied.
    pub source_path: *mut c_char,
    /// False for additions, in which case `old_id` is meaningless.
    pub has_old: bool,
    /// The blob before the change.
    pub old_id: RcGitOid,
    /// False for deletions, in which case `new_id` is meaningless.
    pub has_new: bool,
    /// The blob after the change.
    pub new_id: RcGitOid,
    /// What the entry is after the change, or was before it for deletions.
    pub entry: RcGitEntryKind,
}

impl RcGitTreeChange {
    fn export(change: TreeChange) -> Self {
        Self {
            kind: change.kind.into(),
            path: export_string(Vec::from(change.path)),
            source_path: change
                .source_path
                .map_or(ptr::null_mut(), |path| export_string(Vec::from(path))),
            has_old: change.old.is_some(),
            old_id: change.old.map_or_else(RcGitOid::empty, RcGitOid::from),
            has_new: change.new.is_some(),
            new_id: change.new.map_or_else(RcGitOid::empty, RcGitOid::from),
            entry: change.entry.into(),
        }
    }
}

impl Drop for RcGitTreeChange {
    fn drop(&mut self) {
        // SAFETY: both strings were produced by `export_string` and are released once here.
        unsafe {
            release_string(self.path);
            release_string(self.source_path);
        }
    }
}

/// Every changed file between two trees, sorted by path.
#[repr(C)]
pub struct RcGitTreeChangeList {
    /// `len` changes.
    pub changes: *mut RcGitTreeChange,
    /// Number of changes.
    pub len: usize,
}

impl Drop for RcGitTreeChangeList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.changes, self.len) };
    }
}

/// The role of one line in a diff hunk.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitDiffLineKind {
    /// Unchanged, shown for context.
    Context = 0,
    /// Present only in the new blob.
    Added = 1,
    /// Present only in the old blob.
    Removed = 2,
}

impl From<DiffLineKind> for RcGitDiffLineKind {
    fn from(kind: DiffLineKind) -> Self {
        match kind {
            DiffLineKind::Context => Self::Context,
            DiffLineKind::Added => Self::Added,
            DiffLineKind::Removed => Self::Removed,
        }
    }
}

/// A line of a diff hunk, without its line terminator.
#[repr(C)]
pub struct RcGitDiffLine {
    /// Context, added, or removed.
    pub kind: RcGitDiffLineKind,
    /// The bytes of the line.
    pub text: *mut c_char,
}

impl Drop for RcGitDiffLine {
    fn drop(&mut self) {
        // SAFETY: `text` was produced by `export_string` and is released once here.
        unsafe { release_string(self.text) };
    }
}

/// A run of changed lines with its surrounding context.
#[repr(C)]
pub struct RcGitDiffHunk {
    /// 1-based first line of the hunk in the old blob.
    pub old_start: u32,
    /// Number of old-blob lines the hunk covers.
    pub old_lines: u32,
    /// 1-based first line of the hunk in the new blob.
    pub new_start: u32,
    /// Number of new-blob lines the hunk covers.
    pub new_lines: u32,
    /// `line_count` lines, old and new interleaved.
    pub lines: *mut RcGitDiffLine,
    /// Number of lines.
    pub line_count: usize,
}

impl RcGitDiffHunk {
    fn export(hunk: DiffHunk) -> Self {
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines {
            lines.push(RcGitDiffLine {
                kind: line.kind.into(),
                text: export_string(Vec::from(line.text)),
            });
        }
        let (lines, line_count) = export_vec(lines);
        Self {
            old_start: hunk.old_start,
            old_lines: hunk.old_lines,
            new_start: hunk.new_start,
            new_lines: hunk.new_lines,
            lines,
            line_count,
        }
    }
}

impl Drop for RcGitDiffHunk {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.lines, self.line_count) };
    }
}

/// What a blob diff produced.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitBlobDiffKind {
    /// `hunks` is filled; empty means the blobs are identical.
    Text = 0,
    /// One side is binary; `old_size` and `new_size` are filled.
    Binary = 1,
    /// One side is over the limit; `size` and `limit` are filled.
    TooLarge = 2,
}

/// Line diff between two blobs.
#[repr(C)]
pub struct RcGitBlobDiff {
    /// Which fields are meaningful.
    pub kind: RcGitBlobDiffKind,
    /// `hunk_count` hunks. Text only.
    pub hunks: *mut RcGitDiffHunk,
    /// Number of hunks.
    pub hunk_count: usize,
    /// Size of the old blob, 0 when absent. Binary only.
    pub old_size: u64,
    /// Size of the new blob, 0 when absent. Binary only.
    pub new_size: u64,
    /// Size of the side that exceeded the limit. `TooLarge` only.
    pub size: u64,
    /// The limit that was exceeded. `TooLarge` only.
    pub limit: u64,
}

impl Drop for RcGitBlobDiff {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.hunks, self.hunk_count) };
    }
}

/// A run of consecutive lines introduced by one commit.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcGitBlameRange {
    /// The commit that introduced the lines.
    pub commit: RcGitOid,
    /// 1-based first line of the run in the blamed file.
    pub start_line: u32,
    /// Number of lines in the run.
    pub line_count: u32,
}

/// Every blame range of one file, ordered by line number.
#[repr(C)]
pub struct RcGitBlameList {
    /// `len` ranges.
    pub ranges: *mut RcGitBlameRange,
    /// Number of ranges.
    pub len: usize,
}

impl Drop for RcGitBlameList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.ranges, self.len) };
    }
}

/// How a file on disk relates to the same path in the compared tree.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitWorktreeStatus {
    /// Content and kind match the tree entry.
    Unchanged = 0,
    /// The path exists in both but content or kind differs.
    Modified = 1,
    /// The path exists only on disk.
    Untracked = 2,
    /// The path exists only on disk and matches an ignore rule.
    Ignored = 3,
    /// The path exists only in the compared tree; the file was deleted from disk.
    Deleted = 4,
}

impl From<WorktreeStatus> for RcGitWorktreeStatus {
    fn from(status: WorktreeStatus) -> Self {
        match status {
            WorktreeStatus::Unchanged => Self::Unchanged,
            WorktreeStatus::Modified => Self::Modified,
            WorktreeStatus::Untracked => Self::Untracked,
            WorktreeStatus::Ignored => Self::Ignored,
            WorktreeStatus::Deleted => Self::Deleted,
        }
    }
}

/// One entry of a working-tree directory listing.
#[repr(C)]
pub struct RcGitWorktreeEntry {
    /// File name as stored on disk; may be non-UTF-8.
    pub name: *mut c_char,
    /// What the entry is on disk.
    pub kind: RcGitEntryKind,
    /// How it relates to the compared tree.
    pub status: RcGitWorktreeStatus,
    /// False when the compared tree has no blob at this path.
    pub has_oid: bool,
    /// The blob at the same path in the compared tree.
    pub oid: RcGitOid,
    /// Size in bytes; 0 for directories.
    pub size: u64,
}

impl Drop for RcGitWorktreeEntry {
    fn drop(&mut self) {
        // SAFETY: `name` was produced by `export_string` and is released once here.
        unsafe { release_string(self.name) };
    }
}

/// A working-tree directory: entries on disk plus tree entries missing from disk.
#[repr(C)]
pub struct RcGitWorktreeList {
    /// `len` entries on disk, ordered for display.
    pub entries: *mut RcGitWorktreeEntry,
    /// Number of entries.
    pub len: usize,
    /// `missing_len` tree entries with no counterpart on disk: deleted files.
    pub missing: *mut RcGitTreeEntry,
    /// Number of missing entries.
    pub missing_len: usize,
}

impl Drop for RcGitWorktreeList {
    fn drop(&mut self) {
        // SAFETY: both arrays were produced by `export_vec` with these lengths.
        unsafe {
            release_vec(self.entries, self.len);
            release_vec(self.missing, self.missing_len);
        }
    }
}

/// One changed path from a full working-tree scan.
#[repr(C)]
pub struct RcGitStatusEntry {
    /// Repository-relative path with `/` separators; may be non-UTF-8.
    pub path: *mut c_char,
    /// The kind on disk; the tree's kind for a deleted path.
    pub kind: RcGitEntryKind,
    /// Always modified, untracked, or deleted; clean and ignored paths are not listed.
    pub status: RcGitWorktreeStatus,
    /// False when the compared tree has no blob at this path.
    pub has_oid: bool,
    /// The blob at the same path in the compared tree.
    pub oid: RcGitOid,
    /// Size on disk in bytes; 0 for deleted files.
    pub size: u64,
}

impl Drop for RcGitStatusEntry {
    fn drop(&mut self) {
        // SAFETY: `path` was produced by `export_string` and is released once here.
        unsafe { release_string(self.path) };
    }
}

/// Every changed path in the working tree, with the walk's own cost accounting.
#[repr(C)]
pub struct RcGitStatusList {
    /// `len` changed paths, sorted by path.
    pub entries: *mut RcGitStatusEntry,
    /// Number of entries.
    pub len: usize,
    /// Entries visited before the walk finished or hit its cap.
    pub scanned: usize,
    /// True when the walk stopped at the cap before covering the whole tree.
    pub truncated: bool,
}

impl Drop for RcGitStatusList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.entries, self.len) };
    }
}

/// One stashed change set, as `git stash list` shows it.
#[repr(C)]
pub struct RcGitStashEntry {
    /// Position in the stash, so 0 is `stash@{0}`, the newest.
    pub index: usize,
    /// The stash commit.
    pub oid: RcGitOid,
    /// The reflog message, e.g. `WIP on main: 1a2b3c4 Subject`.
    pub message: *mut c_char,
}

impl Drop for RcGitStashEntry {
    fn drop(&mut self) {
        // SAFETY: `message` was produced by `export_string` and is released once here.
        unsafe { release_string(self.message) };
    }
}

/// Every stash entry, newest first.
#[repr(C)]
pub struct RcGitStashList {
    /// `len` stash entries, newest first.
    pub entries: *mut RcGitStashEntry,
    /// Number of entries.
    pub len: usize,
}

impl Drop for RcGitStashList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.entries, self.len) };
    }
}

/// What a highlighted region means, for the view to color.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitHighlightClass {
    /// `@attribute`, decorators, derives.
    Attribute = 0,
    /// Comments.
    Comment = 1,
    /// Named constants and enum cases.
    Constant = 2,
    /// Function and method names.
    Function = 3,
    /// Language keywords.
    Keyword = 4,
    /// Numeric literals.
    Number = 5,
    /// Operators.
    Operator = 6,
    /// Object keys and struct fields.
    Property = 7,
    /// Brackets and delimiters.
    Punctuation = 8,
    /// String and character literals.
    String = 9,
    /// Type names.
    Type = 10,
    /// Variables and parameters.
    Variable = 11,
}

impl From<Class> for RcGitHighlightClass {
    fn from(class: Class) -> Self {
        match class {
            Class::Attribute => Self::Attribute,
            Class::Comment => Self::Comment,
            Class::Constant => Self::Constant,
            Class::Function => Self::Function,
            Class::Keyword => Self::Keyword,
            Class::Number => Self::Number,
            Class::Operator => Self::Operator,
            Class::Property => Self::Property,
            Class::Punctuation => Self::Punctuation,
            Class::String => Self::String,
            Class::Type => Self::Type,
            Class::Variable => Self::Variable,
        }
    }
}

/// One highlighted region: byte offsets into the highlighted data, half-open.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcGitHighlightSpan {
    /// First byte of the region.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
    /// What to color it as.
    pub kind: RcGitHighlightClass,
}

/// Every highlight span of one file, ordered by start.
#[repr(C)]
pub struct RcGitHighlightList {
    /// `len` spans.
    pub spans: *mut RcGitHighlightSpan,
    /// Number of spans.
    pub len: usize,
}

impl Drop for RcGitHighlightList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.spans, self.len) };
    }
}

/// The policy's answer for a proposed action.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitDecisionKind {
    /// Run it; no challenge.
    Allow = 0,
    /// Run it after the user answers the challenge.
    Confirm = 1,
    /// Never run it from rc-git; `reason` says why.
    Deny = 2,
}

/// How much an action can lose.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitRisk {
    /// Cannot lose work.
    Safe = 0,
    /// Loses something git still remembers.
    Recoverable = 1,
    /// Loses something nothing remembers.
    Destructive = 2,
}

impl From<Risk> for RcGitRisk {
    fn from(risk: Risk) -> Self {
        match risk {
            Risk::Safe => Self::Safe,
            Risk::Recoverable => Self::Recoverable,
            Risk::Destructive => Self::Destructive,
        }
    }
}

/// What the front end must collect before an action may run. Fields beyond `kind` are
/// meaningful only for the kinds their comments name.
#[repr(C)]
pub struct RcGitGuardDecision {
    /// Allow, Confirm, or Deny.
    pub kind: RcGitDecisionKind,
    /// How much the action can lose. Confirm only.
    pub risk: RcGitRisk,
    /// One sentence saying what will happen. Confirm only.
    pub summary: *mut c_char,
    /// Text the user must type exactly, or NULL when a plain confirm button is enough.
    /// Confirm only.
    pub typed_phrase: *mut c_char,
    /// Whether a backup ref is written before the action runs. Confirm only.
    pub backup_ref: bool,
    /// Why the action is refused. Deny only.
    pub reason: *mut c_char,
    /// Whether every commit of the branch is reachable elsewhere. Branch deletion only.
    pub merged: bool,
}

impl RcGitGuardDecision {
    fn export(decision: Decision, merged: bool) -> Self {
        match decision {
            Decision::Allow => Self {
                kind: RcGitDecisionKind::Allow,
                risk: RcGitRisk::Safe,
                summary: ptr::null_mut(),
                typed_phrase: ptr::null_mut(),
                backup_ref: false,
                reason: ptr::null_mut(),
                merged,
            },
            Decision::Confirm(challenge) => Self {
                kind: RcGitDecisionKind::Confirm,
                risk: challenge.risk.into(),
                summary: export_string(challenge.summary),
                typed_phrase: challenge
                    .typed_phrase
                    .map_or(ptr::null_mut(), export_string),
                backup_ref: challenge.backup_ref,
                reason: ptr::null_mut(),
                merged,
            },
            Decision::Deny { reason } => Self {
                kind: RcGitDecisionKind::Deny,
                risk: RcGitRisk::Destructive,
                summary: ptr::null_mut(),
                typed_phrase: ptr::null_mut(),
                backup_ref: false,
                reason: export_string(reason),
                merged,
            },
        }
    }
}

impl Drop for RcGitGuardDecision {
    fn drop(&mut self) {
        // SAFETY: every string was produced by `export_string` and is released once here.
        unsafe {
            release_string(self.summary);
            release_string(self.typed_phrase);
            release_string(self.reason);
        }
    }
}

impl RcGitOid {
    fn empty() -> Self {
        Self {
            bytes: [0; 32],
            len: 0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pointer helpers. Each `unsafe fn` states the contract the extern functions forward to callers.
// ---------------------------------------------------------------------------------------------

/// Writable out-parameter. Built once per call so writes need no further unsafe blocks.
struct Out<T>(*mut T);

impl<T> Out<T> {
    /// # Safety
    /// `ptr` is NULL or valid for writes of `T` for the lifetime of the returned value.
    unsafe fn new(ptr: *mut T) -> Self {
        Self(ptr)
    }

    fn set(&self, value: T) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: `new` guarantees the pointer is valid for writes when not NULL.
        unsafe { self.0.write(value) };
    }
}

impl Out<*mut c_char> {
    fn report(&self, message: impl Display) {
        if self.0.is_null() {
            return;
        }
        self.set(export_string(message.to_string()));
    }
}

/// # Safety
/// `ptr` is NULL or points to a NUL-terminated string that outlives the returned slice.
unsafe fn bytes_arg<'a>(ptr: *const c_char) -> Option<&'a [u8]> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    Some(unsafe { CStr::from_ptr(ptr) }.to_bytes())
}

/// # Safety
/// `repo` is NULL or a pointer from `rcgit_repo_open` that has not been freed.
unsafe fn repo_arg<'a>(repo: *const RcGitRepo) -> Option<&'a Repository> {
    if repo.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    Some(&unsafe { &*repo }.0)
}

/// # Safety
/// `oid` is NULL or points to a readable `RcGitOid`.
unsafe fn oid_arg(oid: *const RcGitOid) -> Option<Oid> {
    if oid.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    unsafe { *oid }.to_oid()
}

/// Distinguishes an intentionally absent id from a malformed one: NULL means "no blob here"
/// and yields `Ok(None)`; a readable id yields `Ok(Some(_))`; a malformed id yields `Err(())`.
///
/// # Safety
/// `oid` is NULL or points to a readable `RcGitOid`.
unsafe fn optional_oid_arg(oid: *const RcGitOid) -> Result<Option<Oid>, ()> {
    if oid.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from this function's contract.
    unsafe { oid_arg(oid) }.map(Some).ok_or(())
}

fn export_string(bytes: impl Into<Vec<u8>>) -> *mut c_char {
    let mut bytes = bytes.into();
    bytes.retain(|&byte| byte != 0);
    CString::new(bytes).map_or(ptr::null_mut(), CString::into_raw)
}

/// # Safety
/// `s` is NULL or came from `export_string` and is not released twice.
unsafe fn release_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: forwarded from this function's contract.
    drop(unsafe { CString::from_raw(s) });
}

fn export_vec<T>(items: Vec<T>) -> (*mut T, usize) {
    let boxed = items.into_boxed_slice();
    let len = boxed.len();
    (Box::into_raw(boxed).cast::<T>(), len)
}

/// # Safety
/// `ptr` is NULL or came from `export_vec` together with `len`, and is not released twice.
unsafe fn release_vec<T>(ptr: *mut T, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: forwarded from this function's contract.
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, len)) });
}

fn export<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// # Safety
/// `ptr` is NULL or came from `export` and is not released twice.
unsafe fn release<T>(ptr: *mut T) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: forwarded from this function's contract.
    drop(unsafe { Box::from_raw(ptr) });
}

// ---------------------------------------------------------------------------------------------
// Exported functions.
// ---------------------------------------------------------------------------------------------

/// Version of the library, as a static string. Do not free.
#[unsafe(no_mangle)]
pub extern "C" fn rcgit_version() -> *const c_char {
    const VERSION: &CStr =
        match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
            Ok(version) => version,
            Err(_) => c"0.0.0",
        };
    VERSION.as_ptr()
}

/// Releases a string returned by any `rcgit_*` function or stored into an `error` parameter.
///
/// # Safety
/// `s` is NULL or a string this library returned, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_string_free(s: *mut c_char) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release_string(s) };
}

/// Opens the repository at `path`, a working directory or a `.git` directory. Only the
/// repository's own config is read; see `docs/safety.md`.
///
/// # Safety
/// `path` is a NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_repo_open(
    path: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitRepo {
    // SAFETY: forwarded from this function's contract.
    let (path, error) = unsafe { (bytes_arg(path), Out::new(error)) };
    let Some(path) = path else {
        error.report("path is NULL");
        return ptr::null_mut();
    };
    match Repository::open(Path::new(OsStr::from_bytes(path))) {
        Ok(repo) => export(RcGitRepo(repo)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a repository. Every value derived from it stays valid; they own their memory.
///
/// # Safety
/// `repo` is NULL or came from `rcgit_repo_open` and is not freed twice or used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_repo_free(repo: *mut RcGitRepo) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(repo) };
}

/// The `.git` directory. Release with `rcgit_string_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_repo_git_dir(repo: *const RcGitRepo) -> *mut c_char {
    // SAFETY: forwarded from this function's contract.
    let Some(repo) = (unsafe { repo_arg(repo) }) else {
        return ptr::null_mut();
    };
    export_string(repo.git_dir().as_os_str().as_bytes())
}

/// The working directory, or NULL for a bare repository. Release with `rcgit_string_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_repo_work_dir(repo: *const RcGitRepo) -> *mut c_char {
    // SAFETY: forwarded from this function's contract.
    let Some(repo) = (unsafe { repo_arg(repo) }) else {
        return ptr::null_mut();
    };
    repo.work_dir().map_or(ptr::null_mut(), |dir| {
        export_string(dir.as_os_str().as_bytes())
    })
}

/// Reads `HEAD`. Release with `rcgit_head_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_head(
    repo: *const RcGitRepo,
    error: *mut *mut c_char,
) -> *mut RcGitHead {
    // SAFETY: forwarded from this function's contract.
    let (repo, error) = unsafe { (repo_arg(repo), Out::new(error)) };
    let Some(repo) = repo else {
        error.report("repo is NULL");
        return ptr::null_mut();
    };
    match repo.head() {
        Ok(head) => export(RcGitHead {
            ref_name: head.ref_name.map_or(ptr::null_mut(), export_string),
            has_target: head.target.is_some(),
            target: head.target.map_or(
                RcGitOid {
                    bytes: [0; 32],
                    len: 0,
                },
                RcGitOid::from,
            ),
        }),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_head`.
///
/// # Safety
/// `head` is NULL or came from `rcgit_head` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_head_free(head: *mut RcGitHead) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(head) };
}

/// Lists every reference. Release with `rcgit_ref_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_refs(
    repo: *const RcGitRepo,
    error: *mut *mut c_char,
) -> *mut RcGitRefList {
    // SAFETY: forwarded from this function's contract.
    let (repo, error) = unsafe { (repo_arg(repo), Out::new(error)) };
    let Some(repo) = repo else {
        error.report("repo is NULL");
        return ptr::null_mut();
    };
    let listing = match repo.refs() {
        Ok(listing) => listing,
        Err(err) => {
            error.report(err);
            return ptr::null_mut();
        }
    };
    let mut refs = Vec::with_capacity(listing.refs.len());
    for entry in listing.refs {
        refs.push(RcGitRef {
            full_name: export_string(entry.full_name),
            short_name: export_string(entry.short_name),
            kind: entry.kind.into(),
            target: entry.target.into(),
        });
    }
    let mut broken = Vec::with_capacity(listing.broken.len());
    for entry in listing.broken {
        broken.push(RcGitBrokenRef {
            full_name: export_string(entry.full_name),
            error: export_string(entry.error),
        });
    }
    let (refs, len) = export_vec(refs);
    let (broken, broken_len) = export_vec(broken);
    export(RcGitRefList {
        refs,
        len,
        broken,
        broken_len,
    })
}

/// Releases a value from `rcgit_refs`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_refs` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_ref_list_free(list: *mut RcGitRefList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Lists the direct children of `tree`. Release with `rcgit_tree_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tree` points to a readable `RcGitOid`. `error` is NULL
/// or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_tree_entries(
    repo: *const RcGitRepo,
    tree: *const RcGitOid,
    error: *mut *mut c_char,
) -> *mut RcGitTreeList {
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, error) = unsafe { (repo_arg(repo), oid_arg(tree), Out::new(error)) };
    let (Some(repo), Some(tree)) = (repo, tree) else {
        error.report("repo or tree is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.tree_entries(tree) {
        Ok(entries) => {
            let mut exported = Vec::with_capacity(entries.len());
            for entry in entries {
                exported.push(RcGitTreeEntry::export(entry));
            }
            let (entries, len) = export_vec(exported);
            export(RcGitTreeList { entries, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_tree_entries`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_tree_entries` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_tree_list_free(list: *mut RcGitTreeList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Finds the entry at a `/`-separated `path` below `root`. Returns NULL with `*error` untouched
/// when the path does not exist, NULL with `*error` set on failure. Release with
/// `rcgit_tree_entry_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `root` points to a readable `RcGitOid`. `path` is a
/// NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_entry_at_path(
    repo: *const RcGitRepo,
    root: *const RcGitOid,
    path: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitTreeEntry {
    // SAFETY: forwarded from this function's contract.
    let (repo, root, path, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(root),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let (Some(repo), Some(root), Some(path)) = (repo, root, path) else {
        error.report("repo, root, or path is NULL or malformed");
        return ptr::null_mut();
    };
    let path = String::from_utf8_lossy(path);
    match repo.entry_at_path(root, &path) {
        Ok(Some(entry)) => export(RcGitTreeEntry::export(entry)),
        Ok(None) => ptr::null_mut(),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_entry_at_path`.
///
/// # Safety
/// `entry` is NULL or came from `rcgit_entry_at_path` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_tree_entry_free(entry: *mut RcGitTreeEntry) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(entry) };
}

/// Reads a blob for display, refusing to decode anything larger than `limit` bytes. `limit` is
/// capped at 4 GiB so line offsets fit in 32 bits. Release with `rcgit_blob_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `oid` points to a readable `RcGitOid`. `error` is NULL
/// or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blob_read(
    repo: *const RcGitRepo,
    oid: *const RcGitOid,
    limit: u64,
    error: *mut *mut c_char,
) -> *mut RcGitBlob {
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, error) = unsafe { (repo_arg(repo), oid_arg(oid), Out::new(error)) };
    let (Some(repo), Some(oid)) = (repo, oid) else {
        error.report("repo or oid is NULL or malformed");
        return ptr::null_mut();
    };
    let limit = limit.min(u64::from(u32::MAX));
    let content = match repo.read_blob(oid, limit) {
        Ok(content) => content,
        Err(err) => {
            error.report(err);
            return ptr::null_mut();
        }
    };
    export(export_blob(content, limit))
}

fn export_blob(content: BlobContent, limit: u64) -> RcGitBlob {
    match content {
        BlobContent::Text { data, line_starts } => {
            let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
            let mut starts = Vec::with_capacity(line_starts.len());
            for start in line_starts {
                starts.push(u32::try_from(start).unwrap_or(u32::MAX));
            }
            let (data, data_len) = export_vec(data);
            let (line_starts, line_count) = export_vec(starts);
            RcGitBlob {
                kind: RcGitBlobKind::Text,
                size,
                limit,
                data,
                data_len,
                line_starts,
                line_count,
            }
        }
        BlobContent::Binary { size } => RcGitBlob {
            kind: RcGitBlobKind::Binary,
            size,
            limit,
            data: ptr::null_mut(),
            data_len: 0,
            line_starts: ptr::null_mut(),
            line_count: 0,
        },
        BlobContent::TooLarge { size, limit } => RcGitBlob {
            kind: RcGitBlobKind::TooLarge,
            size,
            limit,
            data: ptr::null_mut(),
            data_len: 0,
            line_starts: ptr::null_mut(),
            line_count: 0,
        },
    }
}

/// Releases a value from `rcgit_blob_read`.
///
/// # Safety
/// `blob` is NULL or came from `rcgit_blob_read` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blob_free(blob: *mut RcGitBlob) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(blob) };
}

/// Size of a blob without decoding it. Returns `RCGIT_OK` and fills `out`, or `RCGIT_ERROR`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `oid` points to a readable `RcGitOid`. `out` is valid
/// for writing a `uint64_t`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blob_size(
    repo: *const RcGitRepo,
    oid: *const RcGitOid,
    out: *mut u64,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, out, error) =
        unsafe { (repo_arg(repo), oid_arg(oid), Out::new(out), Out::new(error)) };
    let (Some(repo), Some(oid)) = (repo, oid) else {
        error.report("repo or oid is NULL or malformed");
        return RCGIT_ERROR;
    };
    match repo.blob_size(oid) {
        Ok(size) => {
            out.set(size);
            RCGIT_OK
        }
        Err(err) => {
            error.report(err);
            RCGIT_ERROR
        }
    }
}

/// Commits reachable from `tip`, newest first, skipping `skip` and returning at most `limit`.
/// Release with `rcgit_commit_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`. `error` is NULL
/// or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_log(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    skip: usize,
    limit: usize,
    error: *mut *mut c_char,
) -> *mut RcGitCommitList {
    // SAFETY: forwarded from this function's contract.
    let (repo, tip, error) = unsafe { (repo_arg(repo), oid_arg(tip), Out::new(error)) };
    let (Some(repo), Some(tip)) = (repo, tip) else {
        error.report("repo or tip is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.log(tip, skip, limit) {
        Ok(commits) => {
            let mut exported = Vec::with_capacity(commits.len());
            for commit in commits {
                exported.push(RcGitCommit::export(commit));
            }
            let (commits, len) = export_vec(exported);
            export(RcGitCommitList { commits, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_log`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_log` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_list_free(list: *mut RcGitCommitList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// One commit. Release with `rcgit_commit_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `oid` points to a readable `RcGitOid`. `error` is NULL
/// or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit(
    repo: *const RcGitRepo,
    oid: *const RcGitOid,
    error: *mut *mut c_char,
) -> *mut RcGitCommit {
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, error) = unsafe { (repo_arg(repo), oid_arg(oid), Out::new(error)) };
    let (Some(repo), Some(oid)) = (repo, oid) else {
        error.report("repo or oid is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.commit(oid) {
        Ok(commit) => export(RcGitCommit::export(commit)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_commit`.
///
/// # Safety
/// `commit` is NULL or came from `rcgit_commit` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_free(commit: *mut RcGitCommit) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(commit) };
}

/// One commit with its full message, committer, and root tree. Release with
/// `rcgit_commit_detail_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `oid` points to a readable `RcGitOid`. `error` is NULL
/// or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_details(
    repo: *const RcGitRepo,
    oid: *const RcGitOid,
    error: *mut *mut c_char,
) -> *mut RcGitCommitDetail {
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, error) = unsafe { (repo_arg(repo), oid_arg(oid), Out::new(error)) };
    let (Some(repo), Some(oid)) = (repo, oid) else {
        error.report("repo or oid is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.commit_details(oid) {
        Ok(detail) => export(RcGitCommitDetail::export(detail)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_commit_details`.
///
/// # Safety
/// `detail` is NULL or came from `rcgit_commit_details` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_detail_free(detail: *mut RcGitCommitDetail) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(detail) };
}

/// Commits reachable from `tip` that changed `path`, newest first. Examines at most
/// `max_scan` commits past `scan_skip` and reports how many it examined, so the caller loops
/// pages until `complete`. Release with `rcgit_commit_page_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`. `path` is a
/// NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_path_history(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    path: *const c_char,
    scan_skip: usize,
    limit: usize,
    max_scan: usize,
    error: *mut *mut c_char,
) -> *mut RcGitCommitPage {
    // SAFETY: forwarded from this function's contract.
    let (repo, tip, path, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tip),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let (Some(repo), Some(tip), Some(path)) = (repo, tip, path) else {
        error.report("repo, tip, or path is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.path_history(
        tip,
        &String::from_utf8_lossy(path),
        scan_skip,
        limit,
        max_scan,
    ) {
        Ok(page) => export(RcGitCommitPage::export(page)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_path_history` or `rcgit_search_commits`.
///
/// # Safety
/// `page` is NULL or came from `rcgit_path_history` or `rcgit_search_commits` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_page_free(page: *mut RcGitCommitPage) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(page) };
}

/// Commits reachable from `tip` that match the filter, newest first. `message` and `author`
/// are nullable case-insensitive substrings; `after` and `before` bound the commit time and
/// are ignored when `i64::MIN` and `i64::MAX` respectively. `after` also terminates the
/// walk. Pages like `rcgit_path_history`. Release with `rcgit_commit_page_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`. `message` and
/// `author` are NULL or NUL-terminated strings. `error` is NULL or valid for writing a
/// `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_search_commits(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    message: *const c_char,
    author: *const c_char,
    after: i64,
    before: i64,
    scan_skip: usize,
    limit: usize,
    max_scan: usize,
    error: *mut *mut c_char,
) -> *mut RcGitCommitPage {
    // SAFETY: forwarded from this function's contract; `message` and `author` are read only
    // when non-NULL.
    let (repo, tip, message, author, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tip),
            (!message.is_null()).then(|| bytes_arg(message)).flatten(),
            (!author.is_null()).then(|| bytes_arg(author)).flatten(),
            Out::new(error),
        )
    };
    let (Some(repo), Some(tip)) = (repo, tip) else {
        error.report("repo or tip is NULL or malformed");
        return ptr::null_mut();
    };
    let filter = CommitFilter {
        message: message.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        author: author.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        after: (after != i64::MIN).then_some(after),
        before: (before != i64::MAX).then_some(before),
    };
    match repo.search_commits(tip, &filter, scan_skip, limit, max_scan) {
        Ok(page) => export(RcGitCommitPage::export(page)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Lists every file that differs between two trees, renames detected, sorted by path. `from`
/// and `to` may be any objects that peel to trees; a NULL `from` stands for the empty tree,
/// which turns a root commit's diff into pure additions. Release with
/// `rcgit_tree_change_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `from` is NULL or points to a readable `RcGitOid`;
/// `to` points to one. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_diff_trees(
    repo: *const RcGitRepo,
    from: *const RcGitOid,
    to: *const RcGitOid,
    error: *mut *mut c_char,
) -> *mut RcGitTreeChangeList {
    // SAFETY: forwarded from this function's contract.
    let (repo, from, to, error) = unsafe {
        (
            repo_arg(repo),
            optional_oid_arg(from),
            oid_arg(to),
            Out::new(error),
        )
    };
    let (Some(repo), Ok(from), Some(to)) = (repo, from, to) else {
        error.report("repo or to is NULL, or an oid is malformed");
        return ptr::null_mut();
    };
    match repo.diff_trees(from, to) {
        Ok(changes) => {
            let mut exported = Vec::with_capacity(changes.len());
            for change in changes {
                exported.push(RcGitTreeChange::export(change));
            }
            let (changes, len) = export_vec(exported);
            export(RcGitTreeChangeList { changes, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_diff_trees`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_diff_trees` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_tree_change_list_free(list: *mut RcGitTreeChangeList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Line diff between two blobs. A NULL `old` or `new` stands for an absent side, as in an
/// added or deleted file; passing both as NULL yields an empty text diff. Each side's size
/// is checked against `limit` before decoding. `context` is the number of unchanged lines
/// around each hunk. Release with `rcgit_blob_diff_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `old` and `new` are each NULL or point to a readable
/// `RcGitOid`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_diff_blobs(
    repo: *const RcGitRepo,
    old: *const RcGitOid,
    new: *const RcGitOid,
    limit: u64,
    context: u32,
    error: *mut *mut c_char,
) -> *mut RcGitBlobDiff {
    // SAFETY: forwarded from this function's contract.
    let (repo, old, new, error) = unsafe {
        (
            repo_arg(repo),
            optional_oid_arg(old),
            optional_oid_arg(new),
            Out::new(error),
        )
    };
    let (Some(repo), Ok(old), Ok(new)) = (repo, old, new) else {
        error.report("repo is NULL or an oid is malformed");
        return ptr::null_mut();
    };
    match repo.diff_blobs(old, new, limit, context) {
        Ok(diff) => export_blob_diff(diff, limit),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Exports any [`BlobDiff`] outcome as one owned `RcGitBlobDiff`.
fn export_blob_diff(diff: BlobDiff, limit: u64) -> *mut RcGitBlobDiff {
    match diff {
        BlobDiff::Text { hunks } => {
            let mut exported = Vec::with_capacity(hunks.len());
            for hunk in hunks {
                exported.push(RcGitDiffHunk::export(hunk));
            }
            let (hunks, hunk_count) = export_vec(exported);
            export(RcGitBlobDiff {
                kind: RcGitBlobDiffKind::Text,
                hunks,
                hunk_count,
                old_size: 0,
                new_size: 0,
                size: 0,
                limit,
            })
        }
        BlobDiff::Binary { old_size, new_size } => export(RcGitBlobDiff {
            kind: RcGitBlobDiffKind::Binary,
            hunks: ptr::null_mut(),
            hunk_count: 0,
            old_size,
            new_size,
            size: 0,
            limit,
        }),
        BlobDiff::TooLarge { size, limit } => export(RcGitBlobDiff {
            kind: RcGitBlobDiffKind::TooLarge,
            hunks: ptr::null_mut(),
            hunk_count: 0,
            old_size: 0,
            new_size: 0,
            size,
            limit,
        }),
    }
}

/// Releases a value from `rcgit_diff_blobs`.
///
/// # Safety
/// `diff` is NULL or came from `rcgit_diff_blobs` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blob_diff_free(diff: *mut RcGitBlobDiff) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(diff) };
}

/// Attributes every line of the file at `path` in commit `tip` to the commit that introduced
/// it. This walks history, so its cost grows with the commits that touched the file; call it
/// for the file on screen, never in bulk. Release with `rcgit_blame_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`. `path` is a
/// NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blame(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    path: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitBlameList {
    // SAFETY: forwarded from this function's contract.
    let (repo, tip, path, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tip),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let (Some(repo), Some(tip), Some(path)) = (repo, tip, path) else {
        error.report("repo, tip, or path is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.blame(tip, &String::from_utf8_lossy(path)) {
        Ok(ranges) => {
            let mut exported = Vec::with_capacity(ranges.len());
            for range in ranges {
                exported.push(RcGitBlameRange {
                    commit: range.commit.into(),
                    start_line: range.start_line,
                    line_count: range.line_count,
                });
            }
            let (ranges, len) = export_vec(exported);
            export(RcGitBlameList { ranges, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_blame`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_blame` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blame_list_free(list: *mut RcGitBlameList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Lists one directory of the working tree with a status overlay against the tree at the
/// same path below `tree`. `dir` is repository-relative; empty or "/" for the root. Release
/// with `rcgit_worktree_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tree` points to a readable `RcGitOid`. `dir` is a
/// NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_entries(
    repo: *const RcGitRepo,
    tree: *const RcGitOid,
    dir: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitWorktreeList {
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, dir, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tree),
            bytes_arg(dir),
            Out::new(error),
        )
    };
    let (Some(repo), Some(tree), Some(dir)) = (repo, tree, dir) else {
        error.report("repo, tree, or dir is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.worktree_entries(tree, &String::from_utf8_lossy(dir)) {
        Ok(listing) => {
            let mut entries = Vec::with_capacity(listing.entries.len());
            for entry in listing.entries {
                entries.push(RcGitWorktreeEntry {
                    name: export_string(Vec::from(entry.name)),
                    kind: entry.kind.into(),
                    status: entry.status.into(),
                    has_oid: entry.oid.is_some(),
                    oid: entry.oid.map_or_else(RcGitOid::empty, RcGitOid::from),
                    size: entry.size,
                });
            }
            let mut missing = Vec::with_capacity(listing.missing.len());
            for entry in listing.missing {
                missing.push(RcGitTreeEntry::export(entry));
            }
            let (entries, len) = export_vec(entries);
            let (missing, missing_len) = export_vec(missing);
            export(RcGitWorktreeList {
                entries,
                len,
                missing,
                missing_len,
            })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_worktree_entries`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_worktree_entries` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_list_free(list: *mut RcGitWorktreeList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Reads one working-tree file for display, with the same size cap and binary probe as
/// `rcgit_blob_read`. A symlink yields its target as text. Release with `rcgit_blob_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `path` is a NUL-terminated string. `error` is NULL or
/// valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_read(
    repo: *const RcGitRepo,
    path: *const c_char,
    limit: u64,
    error: *mut *mut c_char,
) -> *mut RcGitBlob {
    // SAFETY: forwarded from this function's contract.
    let (repo, path, error) = unsafe { (repo_arg(repo), bytes_arg(path), Out::new(error)) };
    let (Some(repo), Some(path)) = (repo, path) else {
        error.report("repo or path is NULL");
        return ptr::null_mut();
    };
    let limit = limit.min(u64::from(u32::MAX));
    match repo.worktree_file(&String::from_utf8_lossy(path), limit) {
        Ok(content) => export(export_blob(content, limit)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Walks the whole working tree against `tree` and returns every changed path: modified,
/// untracked, and deleted files, sorted by path. The walk stops after visiting `max_scan`
/// entries and reports the truncation. Release with `rcgit_status_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tree` points to a readable `RcGitOid`. `error` is
/// NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_status(
    repo: *const RcGitRepo,
    tree: *const RcGitOid,
    max_scan: usize,
    error: *mut *mut c_char,
) -> *mut RcGitStatusList {
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, error) = unsafe { (repo_arg(repo), oid_arg(tree), Out::new(error)) };
    let (Some(repo), Some(tree)) = (repo, tree) else {
        error.report("repo or tree is NULL or malformed");
        return ptr::null_mut();
    };
    match repo.worktree_status(tree, max_scan) {
        Ok(report) => {
            let mut entries = Vec::with_capacity(report.entries.len());
            for entry in report.entries {
                entries.push(RcGitStatusEntry {
                    path: export_string(Vec::from(entry.path)),
                    kind: entry.kind.into(),
                    status: entry.status.into(),
                    has_oid: entry.oid.is_some(),
                    oid: entry.oid.map_or_else(RcGitOid::empty, RcGitOid::from),
                    size: entry.size,
                });
            }
            let (entries, len) = export_vec(entries);
            export(RcGitStatusList {
                entries,
                len,
                scanned: report.scanned,
                truncated: report.truncated,
            })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_worktree_status`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_worktree_status` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_status_list_free(list: *mut RcGitStatusList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Line diff between the blob at `old` and the working-tree file at `path`, both as raw
/// bytes. `old` is NULL for an untracked file, which makes the diff pure additions. Release
/// with `rcgit_blob_diff_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `old` is NULL or points to a readable `RcGitOid`.
/// `path` is a NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_diff_worktree_file(
    repo: *const RcGitRepo,
    old: *const RcGitOid,
    path: *const c_char,
    limit: u64,
    context: u32,
    error: *mut *mut c_char,
) -> *mut RcGitBlobDiff {
    // SAFETY: forwarded from this function's contract.
    let (repo, old, path, error) = unsafe {
        (
            repo_arg(repo),
            optional_oid_arg(old),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let (Some(repo), Ok(old), Some(path)) = (repo, old, path) else {
        error.report("repo or path is NULL, or the oid is malformed");
        return ptr::null_mut();
    };
    match repo.diff_worktree_file(old, &String::from_utf8_lossy(path), limit, context) {
        Ok(diff) => export_blob_diff(diff, limit),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Lists every stash entry, newest first, from the `refs/stash` reflog. A repository with no
/// stash returns an empty list. Release with `rcgit_stash_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_stash_list(
    repo: *const RcGitRepo,
    error: *mut *mut c_char,
) -> *mut RcGitStashList {
    // SAFETY: forwarded from this function's contract.
    let (repo, error) = unsafe { (repo_arg(repo), Out::new(error)) };
    let Some(repo) = repo else {
        error.report("repo is NULL");
        return ptr::null_mut();
    };
    match repo.stash_list() {
        Ok(stashes) => {
            let mut entries = Vec::with_capacity(stashes.len());
            for entry in stashes {
                entries.push(RcGitStashEntry {
                    index: entry.index,
                    oid: entry.oid.into(),
                    message: export_string(entry.message.into_bytes()),
                });
            }
            let (entries, len) = export_vec(entries);
            export(RcGitStashList { entries, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_stash_list`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_stash_list` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_stash_list_free(list: *mut RcGitStashList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Highlights `len` bytes of `data` as the language `name`'s extension implies. Returns NULL
/// with `*error` untouched when the extension maps to no known language, so plain text costs
/// nothing; NULL with `*error` set when parsing fails. Needs no repository. Release with
/// `rcgit_highlight_list_free`.
///
/// # Safety
/// `name` is a NUL-terminated string. `data` points to `len` readable bytes, or `len` is 0.
/// `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_highlight(
    name: *const c_char,
    data: *const u8,
    len: usize,
    error: *mut *mut c_char,
) -> *mut RcGitHighlightList {
    // SAFETY: forwarded from this function's contract.
    let (name, error) = unsafe { (bytes_arg(name), Out::new(error)) };
    let Some(name) = name else {
        error.report("name is NULL");
        return ptr::null_mut();
    };
    let data: &[u8] = if data.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: forwarded from this function's contract.
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    let Some(language) = Language::from_file_name(&String::from_utf8_lossy(name)) else {
        return ptr::null_mut();
    };
    match rc_git_highlight::highlight(data, language) {
        Ok(spans) => {
            let mut exported = Vec::with_capacity(spans.len());
            for span in spans {
                exported.push(RcGitHighlightSpan {
                    start: span.start,
                    end: span.end,
                    kind: span.class.into(),
                });
            }
            let (spans, len) = export_vec(exported);
            export(RcGitHighlightList { spans, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_highlight`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_highlight` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_highlight_list_free(list: *mut RcGitHighlightList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

// ---------------------------------------------------------------------------------------------
// Guarded mutations. The only functions that write to a repository. Each consults the default
// policy inside rc-git-guard; the typed confirmation crosses as `input` and is verified there.
// ---------------------------------------------------------------------------------------------

/// What deleting the branch `name` would need: the policy's decision plus whether the branch
/// is merged. `repo_path` is the repository's path, as given to `rcgit_repo_open`. Release
/// with `rcgit_guard_decision_free`.
///
/// # Safety
/// `repo_path` and `name` are NUL-terminated strings. `error` is NULL or valid for writing
/// a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_plan_branch_delete(
    repo_path: *const c_char,
    name: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitGuardDecision {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, error) =
        unsafe { (bytes_arg(repo_path), bytes_arg(name), Out::new(error)) };
    let (Some(repo_path), Some(name)) = (repo_path, name) else {
        error.report("repo_path or name is NULL");
        return ptr::null_mut();
    };
    let path = Path::new(OsStr::from_bytes(repo_path));
    match plan_branch_delete(path, &Policy::default(), &String::from_utf8_lossy(name)) {
        Ok(plan) => export(RcGitGuardDecision::export(plan.decision, plan.merged)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// What deleting the tag `name` would need. Pure policy; no repository read. Release with
/// `rcgit_guard_decision_free`.
///
/// # Safety
/// `name` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_plan_tag_delete(
    name: *const c_char,
) -> *mut RcGitGuardDecision {
    // SAFETY: forwarded from this function's contract.
    let Some(name) = (unsafe { bytes_arg(name) }) else {
        return ptr::null_mut();
    };
    let decision = plan_tag_delete(&Policy::default(), &String::from_utf8_lossy(name));
    export(RcGitGuardDecision::export(decision, false))
}

/// Releases a value from a `rcgit_guard_plan_*` function.
///
/// # Safety
/// `decision` is NULL or came from a plan function and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_decision_free(decision: *mut RcGitGuardDecision) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(decision) };
}

/// Creates a local branch named `name` at `revspec`, never overwriting an existing one.
/// Returns `RCGIT_OK` or `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path`, `name`, and `revspec` are NUL-terminated strings. `error` is NULL or valid
/// for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_branch_create(
    repo_path: *const c_char,
    name: *const c_char,
    revspec: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, revspec, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(revspec),
            Out::new(error),
        )
    };
    let (Some(repo_path), Some(name), Some(revspec)) = (repo_path, name, revspec) else {
        error.report("repo_path, name, or revspec is NULL");
        return RCGIT_ERROR;
    };
    finish_action(
        create_branch(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            &String::from_utf8_lossy(name),
            &String::from_utf8_lossy(revspec),
        ),
        &error,
    )
}

/// Creates a lightweight tag named `name` at `revspec`, never overwriting an existing one.
/// Returns `RCGIT_OK` or `RCGIT_ERROR`.
///
/// # Safety
/// As `rcgit_guard_branch_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_tag_create(
    repo_path: *const c_char,
    name: *const c_char,
    revspec: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, revspec, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(revspec),
            Out::new(error),
        )
    };
    let (Some(repo_path), Some(name), Some(revspec)) = (repo_path, name, revspec) else {
        error.report("repo_path, name, or revspec is NULL");
        return RCGIT_ERROR;
    };
    finish_action(
        create_tag(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            &String::from_utf8_lossy(name),
            &String::from_utf8_lossy(revspec),
        ),
        &error,
    )
}

/// Deletes the local branch `name`. `input` is what the user typed to answer the challenge,
/// or NULL when the decision needed only a plain confirm; it is verified inside the guard.
/// On success, when a backup ref was written its full name is stored into `backup` (release
/// with `rcgit_string_free`; NULL `backup` is fine). Returns `RCGIT_OK` or `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` and `name` are NUL-terminated strings; `input` is NULL or one. `backup` is
/// NULL or valid for writing a `char *`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_branch_delete(
    repo_path: *const c_char,
    name: *const c_char,
    input: *const c_char,
    backup: *mut *mut c_char,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, input, backup, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(input),
            Out::new(backup),
            Out::new(error),
        )
    };
    let (Some(repo_path), Some(name)) = (repo_path, name) else {
        error.report("repo_path or name is NULL");
        return RCGIT_ERROR;
    };
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    match delete_branch(
        Path::new(OsStr::from_bytes(repo_path)),
        &Policy::default(),
        &String::from_utf8_lossy(name),
        input.as_deref(),
    ) {
        Ok(deleted) => {
            if let Some(backup_ref) = deleted.backup_ref {
                backup.set(export_string(backup_ref));
            }
            RCGIT_OK
        }
        Err(err) => {
            error.report(err);
            RCGIT_ERROR
        }
    }
}

/// Deletes the tag `name`. Same contract as `rcgit_guard_branch_delete`.
///
/// # Safety
/// As `rcgit_guard_branch_delete`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_tag_delete(
    repo_path: *const c_char,
    name: *const c_char,
    input: *const c_char,
    backup: *mut *mut c_char,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, input, backup, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(input),
            Out::new(backup),
            Out::new(error),
        )
    };
    let (Some(repo_path), Some(name)) = (repo_path, name) else {
        error.report("repo_path or name is NULL");
        return RCGIT_ERROR;
    };
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    match delete_tag(
        Path::new(OsStr::from_bytes(repo_path)),
        &Policy::default(),
        &String::from_utf8_lossy(name),
        input.as_deref(),
    ) {
        Ok(deleted) => {
            if let Some(backup_ref) = deleted.backup_ref {
                backup.set(export_string(backup_ref));
            }
            RCGIT_OK
        }
        Err(err) => {
            error.report(err);
            RCGIT_ERROR
        }
    }
}

fn finish_action(result: Result<(), ActionError>, error: &Out<*mut c_char>) -> i32 {
    match result {
        Ok(()) => RCGIT_OK,
        Err(err) => {
            error.report(err);
            RCGIT_ERROR
        }
    }
}

/// Resolves a revision expression (`main`, `v1.0`, `HEAD~2`, an abbreviated hash) to the object
/// it names. Returns `RCGIT_OK` and fills `out`, or `RCGIT_ERROR`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `spec` is a NUL-terminated string. `out` is valid for
/// writing an `RcGitOid`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_resolve(
    repo: *const RcGitRepo,
    spec: *const c_char,
    out: *mut RcGitOid,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo, spec, out, error) = unsafe {
        (
            repo_arg(repo),
            bytes_arg(spec),
            Out::new(out),
            Out::new(error),
        )
    };
    let (Some(repo), Some(spec)) = (repo, spec) else {
        error.report("repo or spec is NULL");
        return RCGIT_ERROR;
    };
    finish_oid(repo.resolve(&String::from_utf8_lossy(spec)), &out, &error)
}

/// Resolves a revision expression and peels it to the tree it contains.
///
/// # Safety
/// As `rcgit_resolve`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_resolve_tree(
    repo: *const RcGitRepo,
    spec: *const c_char,
    out: *mut RcGitOid,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo, spec, out, error) = unsafe {
        (
            repo_arg(repo),
            bytes_arg(spec),
            Out::new(out),
            Out::new(error),
        )
    };
    let (Some(repo), Some(spec)) = (repo, spec) else {
        error.report("repo or spec is NULL");
        return RCGIT_ERROR;
    };
    finish_oid(
        repo.resolve_tree(&String::from_utf8_lossy(spec)),
        &out,
        &error,
    )
}

/// Peels any object to its tree: a tag to its commit, a commit to its tree, a tree to itself.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `oid` points to a readable `RcGitOid`. `out` is valid
/// for writing an `RcGitOid`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_tree_of(
    repo: *const RcGitRepo,
    oid: *const RcGitOid,
    out: *mut RcGitOid,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, out, error) =
        unsafe { (repo_arg(repo), oid_arg(oid), Out::new(out), Out::new(error)) };
    let (Some(repo), Some(oid)) = (repo, oid) else {
        error.report("repo or oid is NULL or malformed");
        return RCGIT_ERROR;
    };
    finish_oid(repo.tree_of(oid), &out, &error)
}

fn finish_oid(
    result: rc_git_core::Result<Oid>,
    out: &Out<RcGitOid>,
    error: &Out<*mut c_char>,
) -> i32 {
    match result {
        Ok(oid) => {
            out.set(oid.into());
            RCGIT_OK
        }
        Err(err) => {
            error.report(err);
            RCGIT_ERROR
        }
    }
}

/// Lowercase hexadecimal form of an id. Release with `rcgit_string_free`.
///
/// # Safety
/// `oid` points to a readable `RcGitOid`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_oid_hex(oid: *const RcGitOid) -> *mut c_char {
    // SAFETY: forwarded from this function's contract.
    let Some(oid) = (unsafe { oid_arg(oid) }) else {
        return ptr::null_mut();
    };
    export_string(oid.to_hex())
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::undocumented_unsafe_blocks,
        reason = "tests exercise the C contract directly"
    )]

    use std::ffi::CString;

    use rc_git_testkit::{BIN_DAT, COMMIT_1_TITLE, COMMIT_2_TITLE, Fixture, MAIN_RS, README};

    use super::*;

    fn open(fx: &Fixture) -> *mut RcGitRepo {
        let path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();
        let repo = unsafe { rcgit_repo_open(path.as_ptr(), &raw mut error) };
        assert!(!repo.is_null(), "{}", take_error(&mut error));
        assert!(error.is_null(), "success must leave error untouched");
        repo
    }

    fn take_error(error: &mut *mut c_char) -> String {
        assert!(!error.is_null(), "expected an error message");
        let message = unsafe { CStr::from_ptr(*error) }
            .to_string_lossy()
            .into_owned();
        unsafe { rcgit_string_free(*error) };
        *error = ptr::null_mut();
        message
    }

    fn text(ptr: *const c_char) -> String {
        assert!(!ptr.is_null());
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    fn take_string(ptr: *mut c_char) -> String {
        let value = text(ptr);
        unsafe { rcgit_string_free(ptr) };
        value
    }

    fn resolve(repo: *const RcGitRepo, spec: &str) -> RcGitOid {
        let spec = CString::new(spec).unwrap();
        let mut out = RcGitOid {
            bytes: [0; 32],
            len: 0,
        };
        let mut error = ptr::null_mut();
        let status = unsafe { rcgit_resolve(repo, spec.as_ptr(), &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        out
    }

    fn hex(oid: &RcGitOid) -> String {
        take_string(unsafe { rcgit_oid_hex(oid) })
    }

    #[test]
    fn version_is_static_and_matches_cargo() {
        assert_eq!(text(rcgit_version()), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn open_reports_null_and_bad_paths() {
        let mut error = ptr::null_mut();
        assert!(unsafe { rcgit_repo_open(ptr::null(), &raw mut error) }.is_null());
        assert_eq!(take_error(&mut error), "path is NULL");

        let fx = Fixture::empty_dir();
        let path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();
        assert!(unsafe { rcgit_repo_open(path.as_ptr(), &raw mut error) }.is_null());
        assert!(take_error(&mut error).starts_with("cannot open git repository at"));

        assert!(
            unsafe { rcgit_repo_open(path.as_ptr(), ptr::null_mut()) }.is_null(),
            "NULL error is fine"
        );
    }

    #[test]
    fn open_exposes_directories() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let git_dir = take_string(unsafe { rcgit_repo_git_dir(repo) });
        assert_eq!(
            Path::new(&git_dir).file_name().and_then(OsStr::to_str),
            Some(".git")
        );
        assert!(!unsafe { rcgit_repo_work_dir(repo) }.is_null());
        assert!(unsafe { rcgit_repo_git_dir(ptr::null()) }.is_null());
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn head_reports_branch_and_commit() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let head = unsafe { rcgit_head(repo, &raw mut error) };
        assert!(!head.is_null());
        assert!(error.is_null());
        let head_ref = unsafe { &*head };
        assert_eq!(text(head_ref.ref_name), "refs/heads/main");
        assert!(head_ref.has_target);
        assert_eq!(hex(&head_ref.target), fx.rev_parse("HEAD"));
        unsafe { rcgit_head_free(head) };

        assert!(unsafe { rcgit_head(ptr::null(), &raw mut error) }.is_null());
        assert_eq!(take_error(&mut error), "repo is NULL");
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn refs_are_exported_with_names_and_kinds() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let list = unsafe { rcgit_refs(repo, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        assert_eq!(list_ref.len, 5);
        assert_eq!(list_ref.broken_len, 0);
        let refs = unsafe { std::slice::from_raw_parts(list_ref.refs, list_ref.len) };
        let main = refs
            .iter()
            .find(|r| text(r.full_name) == "refs/heads/main")
            .unwrap();
        assert_eq!(text(main.short_name), "main");
        assert_eq!(main.kind, RcGitRefKind::LocalBranch);
        assert_eq!(hex(&main.target), fx.rev_parse("main"));
        let tag = refs.iter().find(|r| text(r.short_name) == "v1.0").unwrap();
        assert_eq!(tag.kind, RcGitRefKind::Tag);
        assert_eq!(hex(&tag.target), fx.rev_parse("v1.0^{commit}"));
        assert!(refs.iter().any(|r| r.kind == RcGitRefKind::RemoteBranch));
        unsafe { rcgit_ref_list_free(list) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn tree_entries_and_path_lookup() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let root = resolve(repo, "main^{tree}");
        let mut error = ptr::null_mut();
        let list = unsafe { rcgit_tree_entries(repo, &raw const root, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        let entries = unsafe { std::slice::from_raw_parts(list_ref.entries, list_ref.len) };
        let names: Vec<String> = entries.iter().map(|e| text(e.name)).collect();
        assert_eq!(
            names,
            [
                "docs",
                "src",
                "alpha.txt",
                "bin.dat",
                "link",
                "README.md",
                "Zeta.txt"
            ]
        );
        assert_eq!(entries[0].kind, RcGitEntryKind::Tree);
        assert_eq!(entries[4].kind, RcGitEntryKind::Symlink);
        assert_eq!(hex(&entries[1].oid), fx.rev_parse("main:src"));
        unsafe { rcgit_tree_list_free(list) };

        let path = CString::new("src/main.rs").unwrap();
        let entry =
            unsafe { rcgit_entry_at_path(repo, &raw const root, path.as_ptr(), &raw mut error) };
        assert!(!entry.is_null(), "{}", take_error(&mut error));
        assert!(error.is_null());
        let entry_ref = unsafe { &*entry };
        assert_eq!(text(entry_ref.name), "main.rs");
        assert_eq!(entry_ref.kind, RcGitEntryKind::Blob);
        assert_eq!(hex(&entry_ref.oid), fx.rev_parse("main:src/main.rs"));
        unsafe { rcgit_tree_entry_free(entry) };

        let missing = CString::new("nope").unwrap();
        let entry =
            unsafe { rcgit_entry_at_path(repo, &raw const root, missing.as_ptr(), &raw mut error) };
        assert!(entry.is_null());
        assert!(error.is_null(), "not found is not an error");

        let entry =
            unsafe { rcgit_entry_at_path(repo, ptr::null(), path.as_ptr(), &raw mut error) };
        assert!(entry.is_null());
        assert!(take_error(&mut error).contains("NULL"));

        let blob = resolve(repo, "main:README.md");
        let list = unsafe { rcgit_tree_entries(repo, &raw const blob, &raw mut error) };
        assert!(list.is_null());
        assert!(take_error(&mut error).contains("read tree"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn blob_read_covers_text_binary_and_limits() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();

        let main_rs = resolve(repo, "main:src/main.rs");
        let blob = unsafe { rcgit_blob_read(repo, &raw const main_rs, 1 << 20, &raw mut error) };
        assert!(!blob.is_null(), "{}", take_error(&mut error));
        let blob_ref = unsafe { &*blob };
        assert_eq!(blob_ref.kind, RcGitBlobKind::Text);
        assert_eq!(blob_ref.size, MAIN_RS.len() as u64);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(blob_ref.data, blob_ref.data_len) },
            MAIN_RS
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(blob_ref.line_starts, blob_ref.line_count) },
            [0, 12, 32]
        );
        unsafe { rcgit_blob_free(blob) };

        let bin = resolve(repo, "main:bin.dat");
        let blob = unsafe { rcgit_blob_read(repo, &raw const bin, 1 << 20, &raw mut error) };
        let blob_ref = unsafe { &*blob };
        assert_eq!(blob_ref.kind, RcGitBlobKind::Binary);
        assert_eq!(blob_ref.size, BIN_DAT.len() as u64);
        assert!(blob_ref.data.is_null());
        unsafe { rcgit_blob_free(blob) };

        let readme = resolve(repo, "main:README.md");
        let blob = unsafe { rcgit_blob_read(repo, &raw const readme, 3, &raw mut error) };
        let blob_ref = unsafe { &*blob };
        assert_eq!(blob_ref.kind, RcGitBlobKind::TooLarge);
        assert_eq!((blob_ref.size, blob_ref.limit), (README.len() as u64, 3));
        unsafe { rcgit_blob_free(blob) };

        let mut size = 0;
        let status =
            unsafe { rcgit_blob_size(repo, &raw const readme, &raw mut size, &raw mut error) };
        assert_eq!(status, RCGIT_OK);
        assert_eq!(size, README.len() as u64);

        let tree = resolve(repo, "main^{tree}");
        let blob = unsafe { rcgit_blob_read(repo, &raw const tree, 1 << 20, &raw mut error) };
        assert!(blob.is_null());
        assert!(take_error(&mut error).contains("expected a blob"));
        let status =
            unsafe { rcgit_blob_size(repo, &raw const tree, &raw mut size, &raw mut error) };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("expected a blob"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn log_and_single_commit() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");
        let list = unsafe { rcgit_log(repo, &raw const tip, 0, 10, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        let commits = unsafe { std::slice::from_raw_parts(list_ref.commits, list_ref.len) };
        assert_eq!(commits.len(), 2);
        assert_eq!(text(commits[0].title), COMMIT_2_TITLE);
        assert_eq!(text(commits[1].title), COMMIT_1_TITLE);
        assert_eq!(commits[0].parent_count, 1);
        let parents =
            unsafe { std::slice::from_raw_parts(commits[0].parents, commits[0].parent_count) };
        assert_eq!(parents[0], commits[1].oid);
        assert_eq!(commits[1].parent_count, 0);
        assert_eq!(text(commits[1].author_name), "Test Author");
        assert_eq!(text(commits[1].author_email), "author@example.com");
        assert_eq!(commits[1].author_time, 1_704_067_200);
        assert_eq!(commits[1].author_offset, 0);
        let first = commits[1].oid;
        unsafe { rcgit_commit_list_free(list) };

        let commit = unsafe { rcgit_commit(repo, &raw const first, &raw mut error) };
        assert!(!commit.is_null(), "{}", take_error(&mut error));
        assert_eq!(text(unsafe { &*commit }.title), COMMIT_1_TITLE);
        unsafe { rcgit_commit_free(commit) };

        let page = unsafe { rcgit_log(repo, &raw const tip, 1, 10, &raw mut error) };
        assert_eq!(unsafe { &*page }.len, 1);
        unsafe { rcgit_commit_list_free(page) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn path_history_pages_with_a_scan_cursor() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");
        let file = CString::new("src/lib.rs").unwrap();

        let page = unsafe {
            rcgit_path_history(
                repo,
                &raw const tip,
                file.as_ptr(),
                0,
                10,
                100,
                &raw mut error,
            )
        };
        assert!(!page.is_null(), "{}", take_error(&mut error));
        let page_ref = unsafe { &*page };
        let commits = unsafe { std::slice::from_raw_parts(page_ref.commits, page_ref.len) };
        assert_eq!(commits.len(), 1);
        assert_eq!(text(commits[0].title), COMMIT_2_TITLE);
        assert_eq!(page_ref.scanned, 2);
        assert!(page_ref.complete);
        unsafe { rcgit_commit_page_free(page) };

        let capped = unsafe {
            rcgit_path_history(
                repo,
                &raw const tip,
                file.as_ptr(),
                0,
                10,
                1,
                &raw mut error,
            )
        };
        let capped_ref = unsafe { &*capped };
        assert_eq!(capped_ref.scanned, 1);
        assert!(!capped_ref.complete);
        unsafe { rcgit_commit_page_free(capped) };

        let null = unsafe {
            rcgit_path_history(
                repo,
                &raw const tip,
                ptr::null(),
                0,
                10,
                100,
                &raw mut error,
            )
        };
        assert!(null.is_null());
        assert!(take_error(&mut error).contains("NULL"));
        unsafe { rcgit_commit_page_free(ptr::null_mut()) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn search_commits_filters_and_pages() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");
        let needle = CString::new("docs").unwrap();

        let page = unsafe {
            rcgit_search_commits(
                repo,
                &raw const tip,
                needle.as_ptr(),
                ptr::null(),
                i64::MIN,
                i64::MAX,
                0,
                10,
                100,
                &raw mut error,
            )
        };
        assert!(!page.is_null(), "{}", take_error(&mut error));
        let page_ref = unsafe { &*page };
        let commits = unsafe { std::slice::from_raw_parts(page_ref.commits, page_ref.len) };
        assert_eq!(commits.len(), 1);
        assert_eq!(text(commits[0].title), COMMIT_2_TITLE);
        assert_eq!(page_ref.scanned, 2);
        assert!(page_ref.complete);
        unsafe { rcgit_commit_page_free(page) };

        let all = unsafe {
            rcgit_search_commits(
                repo,
                &raw const tip,
                ptr::null(),
                ptr::null(),
                i64::MIN,
                i64::MAX,
                0,
                10,
                100,
                &raw mut error,
            )
        };
        assert_eq!(unsafe { &*all }.len, 2, "NULL filters match everything");
        unsafe { rcgit_commit_page_free(all) };

        let null = unsafe {
            rcgit_search_commits(
                ptr::null(),
                &raw const tip,
                ptr::null(),
                ptr::null(),
                i64::MIN,
                i64::MAX,
                0,
                10,
                100,
                &raw mut error,
            )
        };
        assert!(null.is_null());
        assert!(take_error(&mut error).contains("NULL"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn commit_details_round_trip() {
        let fx = Fixture::standard();
        fx.commit_with_body("Detail commit", "The body.", "2024-01-04T00:00:00+0000");
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");

        let detail = unsafe { rcgit_commit_details(repo, &raw const tip, &raw mut error) };
        assert!(!detail.is_null(), "{}", take_error(&mut error));
        let detail_ref = unsafe { &*detail };
        assert_eq!(text(detail_ref.summary.title), "Detail commit");
        assert_eq!(text(detail_ref.body), "The body.");
        assert_eq!(text(detail_ref.committer_name), "Test Committer");
        assert_eq!(text(detail_ref.committer_email), "committer@example.com");
        assert_eq!(detail_ref.committer_time, 1_704_326_400);
        assert_eq!(detail_ref.committer_offset, 0);
        let tree = resolve(repo, "main^{tree}");
        assert_eq!(detail_ref.tree, tree);
        unsafe { rcgit_commit_detail_free(detail) };

        let missing = unsafe { rcgit_commit_details(repo, &raw const tree, &raw mut error) };
        assert!(missing.is_null());
        assert!(take_error(&mut error).contains("read commit"));

        let null = unsafe { rcgit_commit_details(ptr::null(), &raw const tip, &raw mut error) };
        assert!(null.is_null());
        assert!(!take_error(&mut error).is_empty());
        unsafe { rcgit_commit_detail_free(ptr::null_mut()) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn resolve_variants_and_failures() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        assert_eq!(hex(&resolve(repo, "v1.0")), fx.rev_parse("v1.0"));

        let spec = CString::new("v1.0").unwrap();
        let mut out = RcGitOid {
            bytes: [0; 32],
            len: 0,
        };
        let mut error = ptr::null_mut();
        let status =
            unsafe { rcgit_resolve_tree(repo, spec.as_ptr(), &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_OK);
        assert_eq!(hex(&out), fx.rev_parse("main^{tree}"));

        let tag = resolve(repo, "v1.0");
        let status = unsafe { rcgit_tree_of(repo, &raw const tag, &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_OK);
        assert_eq!(hex(&out), fx.rev_parse("main^{tree}"));

        let bad = CString::new("nope").unwrap();
        let status = unsafe { rcgit_resolve(repo, bad.as_ptr(), &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).starts_with("'nope' is not a revision"));

        let status = unsafe { rcgit_resolve(repo, spec.as_ptr(), ptr::null_mut(), &raw mut error) };
        assert_eq!(status, RCGIT_OK, "a NULL out pointer is tolerated");
        assert!(error.is_null());

        let status =
            unsafe { rcgit_resolve(ptr::null(), spec.as_ptr(), &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(take_error(&mut error), "repo or spec is NULL");

        let malformed = RcGitOid {
            bytes: [0; 32],
            len: 33,
        };
        let status =
            unsafe { rcgit_tree_of(repo, &raw const malformed, &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("malformed"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn free_functions_accept_null() {
        unsafe {
            rcgit_string_free(ptr::null_mut());
            rcgit_repo_free(ptr::null_mut());
            rcgit_head_free(ptr::null_mut());
            rcgit_ref_list_free(ptr::null_mut());
            rcgit_tree_list_free(ptr::null_mut());
            rcgit_tree_entry_free(ptr::null_mut());
            rcgit_blob_free(ptr::null_mut());
            rcgit_commit_list_free(ptr::null_mut());
            rcgit_commit_free(ptr::null_mut());
            rcgit_tree_change_list_free(ptr::null_mut());
            rcgit_blob_diff_free(ptr::null_mut());
            rcgit_blame_list_free(ptr::null_mut());
            rcgit_worktree_list_free(ptr::null_mut());
            rcgit_highlight_list_free(ptr::null_mut());
            rcgit_guard_decision_free(ptr::null_mut());
        }
        assert!(unsafe { rcgit_oid_hex(ptr::null()) }.is_null());
    }

    #[test]
    fn tree_diff_blob_diff_and_blame() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let old = resolve(repo, "v0.9");
        let new = resolve(repo, "main");

        let list =
            unsafe { rcgit_diff_trees(repo, &raw const old, &raw const new, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        assert_eq!(list_ref.len, 5);
        let changes = unsafe { std::slice::from_raw_parts(list_ref.changes, list_ref.len) };
        let guide = changes
            .iter()
            .find(|c| text(c.path) == "docs/guide.md")
            .unwrap();
        assert_eq!(guide.kind, RcGitChangeKind::Added);
        assert!(!guide.has_old && guide.has_new);
        assert!(guide.source_path.is_null());
        assert_eq!(hex(&guide.new_id), fx.rev_parse("main:docs/guide.md"));
        let added_blob = guide.new_id;
        unsafe { rcgit_tree_change_list_free(list) };

        let diff = unsafe {
            rcgit_diff_blobs(
                repo,
                ptr::null(),
                &raw const added_blob,
                1 << 20,
                3,
                &raw mut error,
            )
        };
        assert!(!diff.is_null(), "{}", take_error(&mut error));
        let diff_ref = unsafe { &*diff };
        assert_eq!(diff_ref.kind, RcGitBlobDiffKind::Text);
        assert_eq!(diff_ref.hunk_count, 1);
        let hunk = unsafe { &*diff_ref.hunks };
        assert_eq!((hunk.old_lines, hunk.new_start, hunk.new_lines), (0, 1, 1));
        let lines = unsafe { std::slice::from_raw_parts(hunk.lines, hunk.line_count) };
        assert_eq!(lines[0].kind, RcGitDiffLineKind::Added);
        assert_eq!(text(lines[0].text), "guide");
        unsafe { rcgit_blob_diff_free(diff) };

        let bin = resolve(repo, "main:bin.dat");
        let diff = unsafe {
            rcgit_diff_blobs(
                repo,
                &raw const bin,
                &raw const added_blob,
                1 << 20,
                3,
                &raw mut error,
            )
        };
        assert_eq!(unsafe { &*diff }.kind, RcGitBlobDiffKind::Binary);
        unsafe { rcgit_blob_diff_free(diff) };

        let path = CString::new("README.md").unwrap();
        let blame = unsafe { rcgit_blame(repo, &raw const new, path.as_ptr(), &raw mut error) };
        assert!(!blame.is_null(), "{}", take_error(&mut error));
        let blame_ref = unsafe { &*blame };
        assert_eq!(blame_ref.len, 1);
        let range = unsafe { &*blame_ref.ranges };
        assert_eq!((range.start_line, range.line_count), (1, 3));
        assert_eq!(hex(&range.commit), fx.rev_parse("v0.9^{commit}"));
        unsafe { rcgit_blame_list_free(blame) };

        let missing = CString::new("nope.txt").unwrap();
        let blame = unsafe { rcgit_blame(repo, &raw const new, missing.as_ptr(), &raw mut error) };
        assert!(blame.is_null());
        assert!(take_error(&mut error).contains("blame file"));

        let list = unsafe { rcgit_diff_trees(repo, ptr::null(), &raw const old, &raw mut error) };
        assert!(!list.is_null(), "NULL from means the empty tree");
        assert_eq!(
            unsafe { &*list }.len,
            3,
            "every file of the first commit is an addition"
        );
        unsafe { rcgit_tree_change_list_free(list) };

        let list = unsafe { rcgit_diff_trees(repo, &raw const old, ptr::null(), &raw mut error) };
        assert!(list.is_null());
        assert!(take_error(&mut error).contains("NULL"));

        let malformed = RcGitOid {
            bytes: [0; 32],
            len: 33,
        };
        let diff = unsafe {
            rcgit_diff_blobs(
                repo,
                &raw const malformed,
                ptr::null(),
                1 << 20,
                3,
                &raw mut error,
            )
        };
        assert!(diff.is_null(), "a malformed oid is not an absent side");
        assert!(take_error(&mut error).contains("malformed"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn worktree_listing_and_file_reads() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tree = resolve(repo, "HEAD^{tree}");
        fx.write("untracked.txt", b"new\n");

        let dir = CString::new("").unwrap();
        let list =
            unsafe { rcgit_worktree_entries(repo, &raw const tree, dir.as_ptr(), &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        assert_eq!(list_ref.missing_len, 0);
        let entries = unsafe { std::slice::from_raw_parts(list_ref.entries, list_ref.len) };
        let untracked = entries
            .iter()
            .find(|entry| text(entry.name) == "untracked.txt")
            .unwrap();
        assert_eq!(untracked.status, RcGitWorktreeStatus::Untracked);
        assert!(!untracked.has_oid);
        let readme = entries
            .iter()
            .find(|entry| text(entry.name) == "README.md")
            .unwrap();
        assert_eq!(readme.status, RcGitWorktreeStatus::Unchanged);
        assert!(readme.has_oid);
        assert_eq!(hex(&readme.oid), fx.rev_parse("HEAD:README.md"));
        unsafe { rcgit_worktree_list_free(list) };

        let path = CString::new("README.md").unwrap();
        let blob = unsafe { rcgit_worktree_read(repo, path.as_ptr(), 1 << 20, &raw mut error) };
        assert!(!blob.is_null(), "{}", take_error(&mut error));
        let blob_ref = unsafe { &*blob };
        assert_eq!(blob_ref.kind, RcGitBlobKind::Text);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(blob_ref.data, blob_ref.data_len) },
            README
        );
        unsafe { rcgit_blob_free(blob) };

        let missing = CString::new("nope.txt").unwrap();
        let blob = unsafe { rcgit_worktree_read(repo, missing.as_ptr(), 1 << 20, &raw mut error) };
        assert!(blob.is_null());
        assert!(take_error(&mut error).contains("nope.txt"));

        let list =
            unsafe { rcgit_worktree_entries(repo, ptr::null(), dir.as_ptr(), &raw mut error) };
        assert!(list.is_null());
        assert!(take_error(&mut error).contains("NULL"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test arranges the fixture's working tree; the library never mutates"
    )]
    fn worktree_status_and_stash_cross_the_boundary() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"stash me\n");
        fx.git(&["stash", "push", "-q", "-m", "kept"]);
        fx.write("alpha.txt", b"omega\n");
        fx.write("untracked.txt", b"new\n");
        std::fs::remove_file(fx.path().join("Zeta.txt")).unwrap();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tree = resolve(repo, "HEAD^{tree}");

        let list = unsafe { rcgit_worktree_status(repo, &raw const tree, 100_000, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        assert!(!list_ref.truncated);
        assert!(list_ref.scanned > 0);
        let entries = unsafe { std::slice::from_raw_parts(list_ref.entries, list_ref.len) };
        let rows: Vec<(String, RcGitWorktreeStatus)> = entries
            .iter()
            .map(|entry| (text(entry.path), entry.status))
            .collect();
        assert_eq!(
            rows,
            [
                ("Zeta.txt".to_owned(), RcGitWorktreeStatus::Deleted),
                ("alpha.txt".to_owned(), RcGitWorktreeStatus::Modified),
                ("untracked.txt".to_owned(), RcGitWorktreeStatus::Untracked),
            ]
        );
        assert!(entries[0].has_oid);
        assert_eq!(hex(&entries[0].oid), fx.rev_parse("HEAD:Zeta.txt"));
        unsafe { rcgit_status_list_free(list) };

        let path = CString::new("alpha.txt").unwrap();
        let old = resolve(repo, "HEAD:alpha.txt");
        let diff = unsafe {
            rcgit_diff_worktree_file(
                repo,
                &raw const old,
                path.as_ptr(),
                1 << 20,
                3,
                &raw mut error,
            )
        };
        assert!(!diff.is_null(), "{}", take_error(&mut error));
        let diff_ref = unsafe { &*diff };
        assert_eq!(diff_ref.kind, RcGitBlobDiffKind::Text);
        assert_eq!(diff_ref.hunk_count, 1);
        unsafe { rcgit_blob_diff_free(diff) };

        let stashes = unsafe { rcgit_stash_list(repo, &raw mut error) };
        assert!(!stashes.is_null(), "{}", take_error(&mut error));
        let stashes_ref = unsafe { &*stashes };
        assert_eq!(stashes_ref.len, 1);
        let stash_entries =
            unsafe { std::slice::from_raw_parts(stashes_ref.entries, stashes_ref.len) };
        assert_eq!(stash_entries[0].index, 0);
        assert!(text(stash_entries[0].message).ends_with("kept"));
        assert_eq!(hex(&stash_entries[0].oid), fx.rev_parse("stash@{0}"));
        unsafe { rcgit_stash_list_free(stashes) };

        let list = unsafe { rcgit_worktree_status(repo, ptr::null(), 100_000, &raw mut error) };
        assert!(list.is_null());
        assert!(take_error(&mut error).contains("NULL"));
        let diff = unsafe {
            rcgit_diff_worktree_file(repo, ptr::null(), ptr::null(), 1 << 20, 3, &raw mut error)
        };
        assert!(diff.is_null());
        assert!(take_error(&mut error).contains("NULL"));
        let stashes = unsafe { rcgit_stash_list(ptr::null(), &raw mut error) };
        assert!(stashes.is_null());
        assert!(take_error(&mut error).contains("NULL"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn highlighting_crosses_the_boundary() {
        let mut error = ptr::null_mut();
        let name = CString::new("main.rs").unwrap();
        let data = b"fn main() {}\n";
        let list =
            unsafe { rcgit_highlight(name.as_ptr(), data.as_ptr(), data.len(), &raw mut error) };
        assert!(!list.is_null());
        assert!(error.is_null());
        let list_ref = unsafe { &*list };
        assert!(list_ref.len > 0);
        let spans = unsafe { std::slice::from_raw_parts(list_ref.spans, list_ref.len) };
        assert!(
            spans
                .iter()
                .any(|span| span.kind == RcGitHighlightClass::Keyword)
        );
        assert!(spans.iter().all(|span| span.start < span.end));
        unsafe { rcgit_highlight_list_free(list) };

        let unknown = CString::new("photo.png").unwrap();
        let list =
            unsafe { rcgit_highlight(unknown.as_ptr(), data.as_ptr(), data.len(), &raw mut error) };
        assert!(list.is_null());
        assert!(error.is_null(), "an unknown language is not an error");

        let list = unsafe { rcgit_highlight(ptr::null(), data.as_ptr(), 0, &raw mut error) };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "name is NULL");
    }

    #[test]
    fn guarded_mutations_plan_verify_and_write_backups() {
        let fx = Fixture::standard();
        let path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();

        let name = CString::new("topic").unwrap();
        let rev = CString::new("main~1").unwrap();
        let status = unsafe {
            rcgit_guard_branch_create(path.as_ptr(), name.as_ptr(), rev.as_ptr(), &raw mut error)
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(fx.rev_parse("refs/heads/topic"), fx.rev_parse("main~1"));

        let feature = CString::new("feature").unwrap();
        let plan = unsafe {
            rcgit_guard_plan_branch_delete(path.as_ptr(), feature.as_ptr(), &raw mut error)
        };
        assert!(!plan.is_null(), "{}", take_error(&mut error));
        let plan_ref = unsafe { &*plan };
        assert_eq!(plan_ref.kind, RcGitDecisionKind::Confirm);
        assert_eq!(plan_ref.risk, RcGitRisk::Destructive);
        assert!(!plan_ref.merged);
        assert_eq!(text(plan_ref.typed_phrase), "feature");
        assert!(plan_ref.backup_ref);
        assert!(plan_ref.reason.is_null());
        unsafe { rcgit_guard_decision_free(plan) };

        let wrong = CString::new("nope").unwrap();
        let mut backup = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_branch_delete(
                path.as_ptr(),
                feature.as_ptr(),
                wrong.as_ptr(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("did not match"));
        assert!(backup.is_null());

        let right = CString::new("feature").unwrap();
        let status = unsafe {
            rcgit_guard_branch_delete(
                path.as_ptr(),
                feature.as_ptr(),
                right.as_ptr(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(take_string(backup), "refs/rc-git/backup/branch/feature");

        let main_name = CString::new("main").unwrap();
        let plan = unsafe {
            rcgit_guard_plan_branch_delete(path.as_ptr(), main_name.as_ptr(), &raw mut error)
        };
        assert!(!plan.is_null());
        assert_eq!(unsafe { &*plan }.kind, RcGitDecisionKind::Deny);
        assert!(!unsafe { &*plan }.reason.is_null());
        unsafe { rcgit_guard_decision_free(plan) };

        let tag = CString::new("v1.0").unwrap();
        let plan = unsafe { rcgit_guard_plan_tag_delete(tag.as_ptr()) };
        assert_eq!(unsafe { &*plan }.kind, RcGitDecisionKind::Confirm);
        assert_eq!(text(unsafe { &*plan }.typed_phrase), "v1.0");
        unsafe { rcgit_guard_decision_free(plan) };
        let mut backup = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_tag_delete(
                path.as_ptr(),
                tag.as_ptr(),
                tag.as_ptr(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(take_string(backup), "refs/rc-git/backup/tag/v1.0");

        let status = unsafe {
            rcgit_guard_branch_create(ptr::null(), name.as_ptr(), rev.as_ptr(), &raw mut error)
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("NULL"));
    }

    #[test]
    fn oid_round_trips_through_the_c_layout() {
        let oid = Oid::from_hex("0123456789abcdef0123456789abcdef01234567").unwrap();
        let exported = RcGitOid::from(oid);
        assert_eq!(exported.len, 20);
        assert_eq!(exported.to_oid(), Some(oid));
        assert_eq!(hex(&exported), oid.to_hex());
        assert_eq!(
            RcGitOid {
                bytes: [0; 32],
                len: 3
            }
            .to_oid(),
            None
        );
    }
}
