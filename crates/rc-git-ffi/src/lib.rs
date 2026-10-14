//! C ABI over `rc-git-core` for the Swift app. `docs/ffi.md` explains how to extend it.
//!
//! Rules every function follows:
//!
//! - A pointer returned by an `rcgit_*` function belongs to the caller until it is passed to the
//!   matching `rcgit_*_free`. Freeing NULL is a no-op.
//! - A `char *` is NUL-terminated. Outbound tree entry names and paths that are not UTF-8 pass
//!   through as raw bytes; ref names are repaired, flagged by `RcGitRef::has_exact_name`.
//!   Inbound path and ref-name arguments are read as UTF-8, lossily.
//! - A function that can fail takes `char **error`. On failure it returns NULL (or `RCGIT_ERROR`)
//!   and, when `error` is not NULL, stores a message to release with `rcgit_string_free`. On
//!   success `*error` is left untouched.
//! - An `RcGitRepo` may be used from any thread, including several at once.

use std::ffi::{CStr, CString, OsStr, c_char};
use std::fmt::Display;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use rc_git_core::{
    Blame, BlobContent, BlobDiff, ChangeKind, CommitDetail, CommitFilter, CommitPage,
    CommitSummary, DiffHunk, DiffLineKind, EntryKind, HistoryWalk, LineEnd, LogCursor, Oid,
    RefKind, Repository, TreeChange, TreeEntry, WorktreeStatus,
};
use rc_git_guard::actions::{
    ActionError, ActionResult, Author, Cancel, CommitRequest, Credentials, Deleted, FileSelection,
    HunkStamp, Identity, RemoteAuth, StashRequest, checkout_branch, commit, configured_identity,
    create_branch, create_tag, delete_branch, delete_tag, discard_files, discard_hunks,
    fetch_upstream, plan_branch_delete, plan_commit, plan_discard, plan_stash_apply,
    plan_stash_drop, plan_stash_pop, plan_stash_push, plan_tag_delete, pull_fast_forward,
    push_upstream, stash_apply, stash_drop, stash_pop, stash_push, upstream_status,
};
use rc_git_guard::{Decision, Policy, Risk};
use rc_git_highlight::{Class, Language};

/// Status for functions that fill an out parameter: success.
pub const RCGIT_OK: i32 = 0;
/// Status for functions that fill an out parameter: failure, see `error`.
pub const RCGIT_ERROR: i32 = -1;
/// Status for remote functions: the host wants credentials it did not get; see `error`.
pub const RCGIT_AUTH_REQUIRED: i32 = -2;
/// Status for remote functions: local and remote history diverged; see `error`.
pub const RCGIT_DIVERGED: i32 = -3;
/// Status for remote functions over ssh: the host's key is not known; `hostkey` holds
/// `"<key type> <SHA256 fingerprint>"` for the user to verify, `error` the message.
pub const RCGIT_HOSTKEY_UNKNOWN: i32 = -4;
/// Status for remote functions: the caller signalled the `RcGitCancel` it passed in.
/// Nothing was left half-applied; the operation stopped between steps.
pub const RCGIT_CANCELLED: i32 = -5;
/// Status for commit and stash push: no author identity, since the repository's own config
/// sets no `user.name`/`user.email` and the call passed no fallback; see `error`. Ask the user
/// for one and retry with it as the fallback.
pub const RCGIT_IDENTITY_REQUIRED: i32 = -6;
/// Status for remote functions over ssh: an encrypted key file stayed locked, with no
/// passphrase or the wrong one; `hostkey` holds the key file's path, `error` the message.
/// Ask for that key's passphrase and retry with it.
pub const RCGIT_PASSPHRASE_REQUIRED: i32 = -7;

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
    const fn empty() -> Self {
        Self {
            bytes: [0; 32],
            len: 0,
        }
    }

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

/// Fails the build when `T` stops being shareable across threads. Raw pointers skip the
/// auto-trait checks, so the opaque handles Swift uses from any thread assert it here.
const fn assert_send_sync<T: Send + Sync>() {}

const _: () = assert_send_sync::<RcGitRepo>();

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
    /// Whether the stored name is valid UTF-8. The core repairs a name that is not, so both
    /// names arrive valid, with U+FFFD replacements, and must not be passed back as a ref.
    pub has_exact_name: bool,
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

/// Children of one tree, ordered for display: directories first, then names
/// ASCII-case-insensitively.
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
    /// Whether `data` is valid UTF-8, so decoding it is lossless. Text only; checked in the
    /// core so no caller re-scans the bytes.
    pub valid_utf8: bool,
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
    /// How many commits the page examined, matched or not.
    pub scanned: usize,
    /// True when the walk reached the end of history within this page.
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
}

impl From<ChangeKind> for RcGitChangeKind {
    fn from(kind: ChangeKind) -> Self {
        match kind {
            ChangeKind::Added => Self::Added,
            ChangeKind::Deleted => Self::Deleted,
            ChangeKind::Modified => Self::Modified,
            ChangeKind::Renamed => Self::Renamed,
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
    /// Where the content came from; NULL unless renamed.
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

/// The terminator a diff line had before it was stripped.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitLineEnd {
    /// `\n`.
    Lf = 0,
    /// `\r\n`.
    CrLf = 1,
    /// None: the last line of a blob that does not end in a newline.
    None = 2,
}

impl From<LineEnd> for RcGitLineEnd {
    fn from(eol: LineEnd) -> Self {
        match eol {
            LineEnd::Lf => Self::Lf,
            LineEnd::CrLf => Self::CrLf,
            LineEnd::None => Self::None,
        }
    }
}

/// A line of a diff hunk: where its bytes start in the hunk's `data`, without the terminator.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcGitDiffLine {
    /// Context, added, or removed.
    pub kind: RcGitDiffLineKind,
    /// How the line ended in its blob.
    pub eol: RcGitLineEnd,
    /// Byte offset in the hunk's `data` where the line begins; it ends where the next line
    /// begins, or at `data_len` for the last line. Clamped to `u32::MAX`.
    pub start: u32,
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
    /// `data_len` bytes: the text of every line, terminators stripped, back to back. Not
    /// NUL-terminated, and interior NULs are kept.
    pub data: *mut u8,
    /// Length of `data`.
    pub data_len: usize,
    /// `line_count` lines, old and new interleaved.
    pub lines: *mut RcGitDiffLine,
    /// Number of lines.
    pub line_count: usize,
}

impl RcGitDiffHunk {
    fn export(hunk: DiffHunk) -> Self {
        let mut size = 0;
        for line in &hunk.lines {
            size += line.text.len();
        }
        let mut data = Vec::with_capacity(size);
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines {
            lines.push(RcGitDiffLine {
                kind: line.kind.into(),
                eol: line.eol.into(),
                start: u32::try_from(data.len()).unwrap_or(u32::MAX),
            });
            data.extend_from_slice(&line.text);
        }
        let (data, data_len) = export_vec(data);
        let (lines, line_count) = export_vec(lines);
        Self {
            old_start: hunk.old_start,
            old_lines: hunk.old_lines,
            new_start: hunk.new_start,
            new_lines: hunk.new_lines,
            data,
            data_len,
            lines,
            line_count,
        }
    }
}

impl Drop for RcGitDiffHunk {
    fn drop(&mut self) {
        // SAFETY: both arrays were produced by `export_vec` with these lengths.
        unsafe {
            release_vec(self.data, self.data_len);
            release_vec(self.lines, self.line_count);
        }
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
    /// The walk stopped at the `since` floor before finding where these lines came from:
    /// `commit` is the first commit older than the floor, and the lines are at least that old.
    pub boundary: bool,
}

/// What a blame produced.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcGitBlameKind {
    /// `ranges` is filled.
    Ranges = 0,
    /// The file is over the limit; `size` and `limit` are filled, no history was walked.
    TooLarge = 1,
}

/// Every blame range of one file, ordered by line number.
#[repr(C)]
pub struct RcGitBlameList {
    /// Which fields are meaningful.
    pub kind: RcGitBlameKind,
    /// `len` ranges. `Ranges` only.
    pub ranges: *mut RcGitBlameRange,
    /// Number of ranges.
    pub len: usize,
    /// Size of the file at the tip in bytes. `TooLarge` only.
    pub size: u64,
    /// The limit passed to `rcgit_blame`.
    pub limit: u64,
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

/// One file on disk from a full working-tree listing.
#[repr(C)]
pub struct RcGitWorktreeFile {
    /// Repository-relative path with `/` separators; may be non-UTF-8.
    pub path: *mut c_char,
    /// What the file is on disk: a blob, an executable blob, or a symlink.
    pub kind: RcGitEntryKind,
}

impl Drop for RcGitWorktreeFile {
    fn drop(&mut self) {
        // SAFETY: `path` was produced by `export_string` and is released once here.
        unsafe { release_string(self.path) };
    }
}

/// Every file on disk, with the walk's own cost accounting.
#[repr(C)]
pub struct RcGitWorktreeFileList {
    /// `len` files, sorted by path.
    pub files: *mut RcGitWorktreeFile,
    /// Number of files.
    pub len: usize,
    /// Entries visited before the walk finished or hit its cap.
    pub scanned: usize,
    /// True when the walk stopped at the cap before covering the whole tree.
    pub truncated: bool,
}

impl Drop for RcGitWorktreeFileList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.files, self.len) };
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
    /// Emphasized prose in markdown.
    Emphasis = 3,
    /// Function and method names.
    Function = 4,
    /// Markdown headings.
    Heading = 5,
    /// Language keywords.
    Keyword = 6,
    /// Link destinations and references.
    Link = 7,
    /// Numeric literals.
    Number = 8,
    /// Operators.
    Operator = 9,
    /// Object keys and struct fields.
    Property = 10,
    /// Brackets and delimiters.
    Punctuation = 11,
    /// String and character literals.
    String = 12,
    /// Strongly emphasized prose in markdown.
    Strong = 13,
    /// Type names.
    Type = 14,
    /// Variables and parameters.
    Variable = 15,
}

impl From<Class> for RcGitHighlightClass {
    fn from(class: Class) -> Self {
        match class {
            Class::Attribute => Self::Attribute,
            Class::Comment => Self::Comment,
            Class::Constant => Self::Constant,
            Class::Emphasis => Self::Emphasis,
            Class::Function => Self::Function,
            Class::Heading => Self::Heading,
            Class::Keyword => Self::Keyword,
            Class::Link => Self::Link,
            Class::Number => Self::Number,
            Class::Operator => Self::Operator,
            Class::Property => Self::Property,
            Class::Punctuation => Self::Punctuation,
            Class::String => Self::String,
            Class::Strong => Self::Strong,
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

/// One clickable region of a source file and the module it leads to: byte offsets into the
/// data that was read, half-open. The specifier is a range of that same data, so no string
/// crosses the boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcGitImportLink {
    /// First byte of the clickable region.
    pub start: u32,
    /// One past its last byte.
    pub end: u32,
    /// First byte of the module specifier, as written between the quotes.
    pub specifier_start: u32,
    /// One past the specifier's last byte.
    pub specifier_end: u32,
}

/// Every import link of one file, ordered by start and never overlapping.
#[repr(C)]
pub struct RcGitImportLinkList {
    /// `len` links.
    pub links: *mut RcGitImportLink,
    /// Number of links.
    pub len: usize,
}

impl Drop for RcGitImportLinkList {
    fn drop(&mut self) {
        // SAFETY: the array was produced by `export_vec` with this length.
        unsafe { release_vec(self.links, self.len) };
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
                risk: challenge.risk().into(),
                summary: export_string(challenge.summary()),
                typed_phrase: challenge
                    .typed_phrase()
                    .map_or(ptr::null_mut(), export_string),
                backup_ref: challenge.backup_ref(),
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

    /// The value of the required argument `arg` of `op`, or `None` after reporting it NULL.
    fn required<T>(&self, op: &str, arg: &str, value: Option<T>) -> Option<T> {
        if value.is_none() {
            self.report(bad_arg(op, arg, &Fault::Null));
        }
        value
    }

    /// The value of the argument `arg` of `op`, or `None` after reporting why it was refused.
    fn valid<T>(&self, op: &str, arg: &str, value: Result<T, Fault>) -> Option<T> {
        self.read(op, value.map_err(|fault| BadArg::new(arg, fault)))
    }

    /// The value read from a nested argument of `op`, or `None` after reporting the part of
    /// it that was refused.
    fn read<T>(&self, op: &str, value: Result<T, BadArg>) -> Option<T> {
        match value {
            Ok(value) => Some(value),
            Err(bad) => {
                self.report(bad_arg(op, &bad.arg, &bad.fault));
                None
            }
        }
    }
}

/// Why a caller's argument was refused.
#[derive(Debug)]
enum Fault {
    /// A required pointer was NULL.
    Null,
    /// An `RcGitOid` whose `len` names no supported hash.
    Malformed { len: usize },
    /// A list that must hold at least one entry held none.
    Empty,
}

impl Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Malformed { len } => write!(f, "malformed (len {len})"),
            Self::Empty => f.write_str("empty"),
        }
    }
}

/// A refused part of a nested argument, named by its path from the parameter
/// (`request.files[2].path`).
#[derive(Debug)]
struct BadArg {
    arg: String,
    fault: Fault,
}

impl BadArg {
    fn new(arg: impl Into<String>, fault: Fault) -> Self {
        Self {
            arg: arg.into(),
            fault,
        }
    }
}

/// The message for an argument the caller got wrong: the function, the argument, and why,
/// as in `rcgit_blob_read: oid is malformed (len 33)`.
fn bad_arg(op: &str, arg: &str, fault: &Fault) -> String {
    format!("{op}: {arg} is {fault}")
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

/// The `len` bytes at `ptr`: empty when `len` is 0, whatever `ptr` is, and `None` when `ptr`
/// is NULL with a nonzero `len`.
///
/// # Safety
/// `ptr` is NULL or points to `len` readable bytes that outlive the returned slice.
unsafe fn slice_arg<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if ptr.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
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
unsafe fn oid_arg(oid: *const RcGitOid) -> Result<Oid, Fault> {
    if oid.is_null() {
        return Err(Fault::Null);
    }
    // SAFETY: forwarded from this function's contract.
    let oid = unsafe { *oid };
    oid.to_oid().ok_or(Fault::Malformed {
        len: usize::from(oid.len),
    })
}

/// Distinguishes an intentionally absent id from a malformed one: NULL means "no blob here"
/// and yields `Ok(None)`; a readable id yields `Ok(Some(_))`; a malformed id yields
/// `Err(Fault::Malformed)`.
///
/// # Safety
/// `oid` is NULL or points to a readable `RcGitOid`.
unsafe fn optional_oid_arg(oid: *const RcGitOid) -> Result<Option<Oid>, Fault> {
    if oid.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from this function's contract.
    unsafe { oid_arg(oid) }.map(Some)
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
    let Some(path) = error.required("rcgit_repo_open", "path", path) else {
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

/// This worktree's git directory: `.git`, or `.git/worktrees/<name>` for a linked worktree.
/// Release with `rcgit_string_free`.
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

/// The git directory every worktree shares, where branches, tags and `packed-refs` live.
/// Equal to `rcgit_repo_git_dir` except in a linked worktree. Release with `rcgit_string_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_repo_common_dir(repo: *const RcGitRepo) -> *mut c_char {
    // SAFETY: forwarded from this function's contract.
    let Some(repo) = (unsafe { repo_arg(repo) }) else {
        return ptr::null_mut();
    };
    export_string(repo.common_dir().as_os_str().as_bytes())
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
    let Some(repo) = error.required("rcgit_head", "repo", repo) else {
        return ptr::null_mut();
    };
    match repo.head() {
        Ok(head) => export(RcGitHead {
            ref_name: head.ref_name.map_or(ptr::null_mut(), export_string),
            has_target: head.target.is_some(),
            target: head.target.map_or_else(RcGitOid::empty, RcGitOid::from),
        }),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// The repository's mainline branch without `refs/heads/`: what a remote's `HEAD` names, else
/// whichever of `master` and `main` exists locally. Returns NULL without setting `error` when
/// nothing names one, and NULL with `error` set on failure. Release with `rcgit_string_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_default_branch(
    repo: *const RcGitRepo,
    error: *mut *mut c_char,
) -> *mut c_char {
    // SAFETY: forwarded from this function's contract.
    let (repo, error) = unsafe { (repo_arg(repo), Out::new(error)) };
    let Some(repo) = error.required("rcgit_default_branch", "repo", repo) else {
        return ptr::null_mut();
    };
    match repo.default_branch() {
        Ok(Some(branch)) => export_string(branch),
        Ok(None) => ptr::null_mut(),
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
    let Some(repo) = error.required("rcgit_refs", "repo", repo) else {
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
            has_exact_name: entry.exact_name,
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
    const OP: &str = "rcgit_tree_entries";
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, error) = unsafe { (repo_arg(repo), oid_arg(tree), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tree) = error.valid(OP, "tree", tree) else {
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
    const OP: &str = "rcgit_entry_at_path";
    // SAFETY: forwarded from this function's contract.
    let (repo, root, path, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(root),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(root) = error.valid(OP, "root", root) else {
        return ptr::null_mut();
    };
    let Some(path) = error.required(OP, "path", path) else {
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
    const OP: &str = "rcgit_blob_read";
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, error) = unsafe { (repo_arg(repo), oid_arg(oid), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(oid) = error.valid(OP, "oid", oid) else {
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
        BlobContent::Text {
            data,
            line_starts,
            valid_utf8,
        } => {
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
                valid_utf8,
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
            valid_utf8: false,
        },
        BlobContent::TooLarge { size, limit } => RcGitBlob {
            kind: RcGitBlobKind::TooLarge,
            size,
            limit,
            data: ptr::null_mut(),
            data_len: 0,
            line_starts: ptr::null_mut(),
            line_count: 0,
            valid_utf8: false,
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
    const OP: &str = "rcgit_blob_size";
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, out, error) =
        unsafe { (repo_arg(repo), oid_arg(oid), Out::new(out), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return RCGIT_ERROR;
    };
    let Some(oid) = error.valid(OP, "oid", oid) else {
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

/// A walk over the commits reachable from one tip, newest first, taken a page at a time.
/// Opaque: create with `rcgit_log_cursor_new`, page with `rcgit_log_cursor_next`, release
/// with `rcgit_log_cursor_free`. Each page resumes where the previous one stopped. It may be
/// used from any thread; concurrent pages are taken one after the other.
pub struct RcGitLogCursor(LogCursor);

const _: () = assert_send_sync::<RcGitLogCursor>();

/// A cursor positioned before the newest commit reachable from `tip`; an annotated tag is
/// followed to its commit. Release with `rcgit_log_cursor_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`. `error` is NULL
/// or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_log_cursor_new(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    error: *mut *mut c_char,
) -> *mut RcGitLogCursor {
    const OP: &str = "rcgit_log_cursor_new";
    // SAFETY: forwarded from this function's contract.
    let (repo, tip, error) = unsafe { (repo_arg(repo), oid_arg(tip), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tip) = error.valid(OP, "tip", tip) else {
        return ptr::null_mut();
    };
    match repo.log_cursor(tip) {
        Ok(cursor) => export(RcGitLogCursor(cursor)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// The next at most `limit` commits of `cursor`, newest first; fewer only at the end of
/// history. Release with `rcgit_commit_list_free`. After a failed page the cursor refuses
/// every later one: start a new cursor.
///
/// # Safety
/// `repo` came from `rcgit_repo_open` and is the repository `cursor` was made from. `cursor`
/// came from `rcgit_log_cursor_new` and has not been freed. `error` is NULL or valid for
/// writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_log_cursor_next(
    repo: *const RcGitRepo,
    cursor: *const RcGitLogCursor,
    limit: usize,
    error: *mut *mut c_char,
) -> *mut RcGitCommitList {
    const OP: &str = "rcgit_log_cursor_next";
    // SAFETY: forwarded from this function's contract.
    let (repo, cursor, error) = unsafe { (repo_arg(repo), opaque_arg(cursor), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(RcGitLogCursor(cursor)) = error.required(OP, "cursor", cursor) else {
        return ptr::null_mut();
    };
    match cursor.next_page(repo, limit) {
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

/// Releases a cursor from `rcgit_log_cursor_new`. The pages it returned stay valid.
///
/// # Safety
/// `cursor` is NULL or came from `rcgit_log_cursor_new`, is not freed twice, and is not in
/// use by a running `rcgit_log_cursor_next`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_log_cursor_free(cursor: *mut RcGitLogCursor) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(cursor) };
}

/// # Safety
/// `handle` is NULL or a pointer from the matching `_new` function that has not been freed.
unsafe fn opaque_arg<'a, T>(handle: *const T) -> Option<&'a T> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    Some(unsafe { &*handle })
}

/// Releases a value from `rcgit_log_cursor_next`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_log_cursor_next` and is not freed twice.
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
    const OP: &str = "rcgit_commit";
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, error) = unsafe { (repo_arg(repo), oid_arg(oid), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(oid) = error.valid(OP, "oid", oid) else {
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
    const OP: &str = "rcgit_commit_details";
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, error) = unsafe { (repo_arg(repo), oid_arg(oid), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(oid) = error.valid(OP, "oid", oid) else {
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

/// Releases a value from `rcgit_history_walk_next`.
///
/// # Safety
/// `page` is NULL or came from `rcgit_history_walk_next` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_page_free(page: *mut RcGitCommitPage) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(page) };
}

/// What `rcgit_history_walk_new` matches: every set field must hold. Caller-owned input,
/// passed by value; mirrors the core's `CommitFilter`.
#[repr(C)]
pub struct RcGitCommitFilter {
    /// Case-insensitive (ASCII) substring of the full message, or NULL for any.
    pub message: *const c_char,
    /// Case-insensitive (ASCII) substring of the author name or email, or NULL for any.
    pub author: *const c_char,
    /// Whether `after` applies.
    pub has_after: bool,
    /// Earliest committer time, seconds since the Unix epoch. Also terminates the walk.
    pub after: i64,
    /// Whether `before` applies.
    pub has_before: bool,
    /// Latest committer time, seconds since the Unix epoch.
    pub before: i64,
    /// Only commits whose entry at this path differs from the first parent's (from every
    /// parent's for a merge; a root or shallow boundary commit matches when the entry exists),
    /// or NULL for any. An empty path names the root tree.
    pub path: *const c_char,
}

/// A filtered walk over the commits reachable from one tip, newest first, taken a page at a
/// time. Opaque: create with `rcgit_history_walk_new`, page with `rcgit_history_walk_next`,
/// release with `rcgit_history_walk_free`. Each page resumes where the previous one
/// stopped. It may be used from any thread; concurrent pages are taken one after the other.
pub struct RcGitHistoryWalk(HistoryWalk);

const _: () = assert_send_sync::<RcGitHistoryWalk>();

/// A walk over the commits reachable from `tip` that match `filter`, positioned before the
/// newest; an annotated tag is followed to its commit. A filter with only `path` set is that
/// path's history. A set `after` ends the walk at that time instead of filtering. Release with
/// `rcgit_history_walk_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`.
/// `filter.message`, `filter.author` and `filter.path` are NULL or NUL-terminated strings.
/// `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_history_walk_new(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    filter: RcGitCommitFilter,
    error: *mut *mut c_char,
) -> *mut RcGitHistoryWalk {
    const OP: &str = "rcgit_history_walk_new";
    // SAFETY: forwarded from this function's contract.
    let (repo, tip, message, author, path, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tip),
            bytes_arg(filter.message),
            bytes_arg(filter.author),
            bytes_arg(filter.path),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tip) = error.valid(OP, "tip", tip) else {
        return ptr::null_mut();
    };
    let core_filter = CommitFilter {
        message: message.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        author: author.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        after: filter.has_after.then_some(filter.after),
        before: filter.has_before.then_some(filter.before),
        path: path.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
    };
    match repo.history_walk(tip, core_filter) {
        Ok(walk) => export(RcGitHistoryWalk(walk)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// The next page of `walk`: at most `limit` matches from at most `max_scan` commits examined,
/// whatever they match, so one call never holds a repository handle through a long scan.
/// Loop until `complete`; a `max_scan` of 0 counts as 1, so every incomplete page advances.
/// Release with `rcgit_commit_page_free`. After a failed page the walk refuses every later
/// one: start a new walk.
///
/// # Safety
/// `repo` came from `rcgit_repo_open` and is the repository `walk` was made from. `walk` came
/// from `rcgit_history_walk_new` and has not been freed. `error` is NULL or valid for
/// writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_history_walk_next(
    repo: *const RcGitRepo,
    walk: *const RcGitHistoryWalk,
    limit: usize,
    max_scan: usize,
    error: *mut *mut c_char,
) -> *mut RcGitCommitPage {
    const OP: &str = "rcgit_history_walk_next";
    // SAFETY: forwarded from this function's contract.
    let (repo, walk, error) = unsafe { (repo_arg(repo), opaque_arg(walk), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(RcGitHistoryWalk(walk)) = error.required(OP, "walk", walk) else {
        return ptr::null_mut();
    };
    match walk.next_page(repo, limit, max_scan) {
        Ok(page) => export(RcGitCommitPage::export(page)),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a walk from `rcgit_history_walk_new`. The pages it returned stay valid.
///
/// # Safety
/// `walk` is NULL or came from `rcgit_history_walk_new`, is not freed twice, and is not in
/// use by a running `rcgit_history_walk_next`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_history_walk_free(walk: *mut RcGitHistoryWalk) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(walk) };
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
    const OP: &str = "rcgit_diff_trees";
    // SAFETY: forwarded from this function's contract.
    let (repo, from, to, error) = unsafe {
        (
            repo_arg(repo),
            optional_oid_arg(from),
            oid_arg(to),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(from) = error.valid(OP, "from", from) else {
        return ptr::null_mut();
    };
    let Some(to) = error.valid(OP, "to", to) else {
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
/// is checked against `limit`, capped at 4 GiB, before decoding. `context` is the number of
/// unchanged lines around each hunk. Release with `rcgit_blob_diff_free`.
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
    const OP: &str = "rcgit_diff_blobs";
    // SAFETY: forwarded from this function's contract.
    let (repo, old, new, error) = unsafe {
        (
            repo_arg(repo),
            optional_oid_arg(old),
            optional_oid_arg(new),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(old) = error.valid(OP, "old", old) else {
        return ptr::null_mut();
    };
    let Some(new) = error.valid(OP, "new", new) else {
        return ptr::null_mut();
    };
    let limit = limit.min(u64::from(u32::MAX));
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
/// it. This walks history, so its cost grows with the history between `tip` and the commit
/// that introduced the oldest surviving line — up to the whole ancestry for a file with a line
/// from the first commit; call it for the file on screen, never in bulk. A file at `tip`
/// larger than `limit` bytes comes back as `TooLarge` after one header read; the versions the
/// walk reaches in history are not size-checked. `since` (seconds since the Unix epoch, or
/// `i64::MIN` for none) is the walk's budget: it never reads past a commit older than it, and
/// the lines still unattributed there come back as `boundary` ranges on that commit. The walk
/// cannot be cancelled once started. Release with `rcgit_blame_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tip` points to a readable `RcGitOid`. `path` is a
/// NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_blame(
    repo: *const RcGitRepo,
    tip: *const RcGitOid,
    path: *const c_char,
    limit: u64,
    since: i64,
    error: *mut *mut c_char,
) -> *mut RcGitBlameList {
    const OP: &str = "rcgit_blame";
    // SAFETY: forwarded from this function's contract.
    let (repo, tip, path, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tip),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tip) = error.valid(OP, "tip", tip) else {
        return ptr::null_mut();
    };
    let Some(path) = error.required(OP, "path", path) else {
        return ptr::null_mut();
    };
    let since = (since != i64::MIN).then_some(since);
    match repo.blame(tip, &String::from_utf8_lossy(path), limit, since) {
        Ok(Blame::TooLarge { size, limit }) => export(RcGitBlameList {
            kind: RcGitBlameKind::TooLarge,
            ranges: ptr::null_mut(),
            len: 0,
            size,
            limit,
        }),
        Ok(Blame::Ranges(ranges)) => {
            let mut exported = Vec::with_capacity(ranges.len());
            for range in ranges {
                exported.push(RcGitBlameRange {
                    commit: range.commit.into(),
                    start_line: range.start_line,
                    line_count: range.line_count,
                    boundary: range.boundary,
                });
            }
            let (ranges, len) = export_vec(exported);
            export(RcGitBlameList {
                kind: RcGitBlameKind::Ranges,
                ranges,
                len,
                size: 0,
                limit,
            })
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
    const OP: &str = "rcgit_worktree_entries";
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, dir, error) = unsafe {
        (
            repo_arg(repo),
            oid_arg(tree),
            bytes_arg(dir),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tree) = error.valid(OP, "tree", tree) else {
        return ptr::null_mut();
    };
    let Some(dir) = error.required(OP, "dir", dir) else {
        return ptr::null_mut();
    };
    match repo.worktree_entries(Some(tree), &String::from_utf8_lossy(dir)) {
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
    const OP: &str = "rcgit_worktree_read";
    // SAFETY: forwarded from this function's contract.
    let (repo, path, error) = unsafe { (repo_arg(repo), bytes_arg(path), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(path) = error.required(OP, "path", path) else {
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
/// untracked, and deleted files, sorted by path. A NULL `tree` means no tree (an unborn
/// branch): every file is untracked. The walk stops after visiting `max_scan` entries and
/// reports the truncation. Release with `rcgit_status_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tree` is NULL or points to a readable `RcGitOid`.
/// `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_status(
    repo: *const RcGitRepo,
    tree: *const RcGitOid,
    max_scan: usize,
    error: *mut *mut c_char,
) -> *mut RcGitStatusList {
    const OP: &str = "rcgit_worktree_status";
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, error) = unsafe { (repo_arg(repo), optional_oid_arg(tree), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tree) = error.valid(OP, "tree", tree) else {
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

/// Walks the whole working tree and returns every file on disk, sorted by path: tracked,
/// untracked, and ignored files alike. An ignored directory that `tree` does not track is
/// pruned without reading it. The walk stops after visiting `max_scan` entries and reports
/// the truncation. Release with `rcgit_worktree_file_list_free`.
///
/// # Safety
/// `repo` came from `rcgit_repo_open`. `tree` points to a readable `RcGitOid`. `error` is
/// NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_files(
    repo: *const RcGitRepo,
    tree: *const RcGitOid,
    max_scan: usize,
    error: *mut *mut c_char,
) -> *mut RcGitWorktreeFileList {
    const OP: &str = "rcgit_worktree_files";
    // SAFETY: forwarded from this function's contract.
    let (repo, tree, error) = unsafe { (repo_arg(repo), oid_arg(tree), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(tree) = error.valid(OP, "tree", tree) else {
        return ptr::null_mut();
    };
    match repo.worktree_files(Some(tree), max_scan) {
        Ok(list) => {
            let mut files = Vec::with_capacity(list.files.len());
            for file in list.files {
                files.push(RcGitWorktreeFile {
                    path: export_string(Vec::from(file.path)),
                    kind: file.kind.into(),
                });
            }
            let (files, len) = export_vec(files);
            export(RcGitWorktreeFileList {
                files,
                len,
                scanned: list.scanned,
                truncated: list.truncated,
            })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_worktree_files`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_worktree_files` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_worktree_file_list_free(list: *mut RcGitWorktreeFileList) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(list) };
}

/// Line diff between the blob at `old` and the working-tree file at `path`, both as raw
/// bytes. `old` is NULL for an untracked file, which makes the diff pure additions. Each
/// side's size is checked against `limit`, capped at 4 GiB, before decoding. Release with
/// `rcgit_blob_diff_free`.
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
    const OP: &str = "rcgit_diff_worktree_file";
    // SAFETY: forwarded from this function's contract.
    let (repo, old, path, error) = unsafe {
        (
            repo_arg(repo),
            optional_oid_arg(old),
            bytes_arg(path),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return ptr::null_mut();
    };
    let Some(old) = error.valid(OP, "old", old) else {
        return ptr::null_mut();
    };
    let Some(path) = error.required(OP, "path", path) else {
        return ptr::null_mut();
    };
    let limit = limit.min(u64::from(u32::MAX));
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
    let Some(repo) = error.required("rcgit_stash_list", "repo", repo) else {
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

/// Highlights `len` bytes of `data` as the language `name` implies: an extension when `name`
/// is a file name, otherwise a markdown fence's info string such as `rust`. Returns NULL with
/// `*error` untouched when `name` maps to no known language, so plain text costs nothing; NULL
/// with `*error` set when `data` is NULL with a nonzero `len` or parsing fails, including
/// when `cancel` was signalled. `cancel` may be NULL when the caller has no way to cancel.
/// Needs no repository. Release with `rcgit_highlight_list_free`.
///
/// # Safety
/// `name` is a NUL-terminated string. `data` is NULL or points to `len` readable bytes.
/// `cancel` is NULL or came from `rcgit_cancel_new` and is not freed before this returns.
/// `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_highlight(
    name: *const c_char,
    data: *const u8,
    len: usize,
    cancel: *const RcGitCancel,
    error: *mut *mut c_char,
) -> *mut RcGitHighlightList {
    // SAFETY: forwarded from this function's contract.
    let (name, data, cancel, error) = unsafe {
        (
            bytes_arg(name),
            slice_arg(data, len),
            cancel_flags(cancel),
            Out::new(error),
        )
    };
    let Some(name) = error.required("rcgit_highlight", "name", name) else {
        return ptr::null_mut();
    };
    let Some(data) = error.required("rcgit_highlight", "data", data) else {
        return ptr::null_mut();
    };
    let name = String::from_utf8_lossy(name);
    let Some(language) = Language::from_file_name(&name).or_else(|| Language::from_fence(&name))
    else {
        return ptr::null_mut();
    };
    let cancel = cancel.map(|cancel| &cancel.highlight);
    match rc_git_highlight::highlight(data, language, cancel) {
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

/// Names the language the file name `name` implies by its extension, as `rcgit_highlight`
/// picks it for a file: `markdown` for `README.md`. Returns NULL with `*error` untouched when
/// `name` maps to no known language; NULL with `*error` set when `name` is NULL. Release
/// with `rcgit_string_free`.
///
/// # Safety
/// `name` is NULL or a NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_language_name(
    name: *const c_char,
    error: *mut *mut c_char,
) -> *mut c_char {
    // SAFETY: forwarded from this function's contract.
    let (name, error) = unsafe { (bytes_arg(name), Out::new(error)) };
    let Some(name) = error.required("rcgit_language_name", "name", name) else {
        return ptr::null_mut();
    };
    Language::from_file_name(&String::from_utf8_lossy(name))
        .map_or(ptr::null_mut(), |language| export_string(language.name()))
}

/// Finds the import links in `len` bytes of `data`, read as the language the file name `name`
/// implies: the specifiers of imports, re-exports, `import()` and `require()`, the names they
/// bind, and the JSX elements named after one. Returns NULL with `*error` untouched when
/// `name` maps to no known language; an empty list when the language has no links or the
/// file has none; NULL with `*error` set when `data` is NULL with a nonzero `len` or parsing
/// fails. Needs no repository. Release with `rcgit_import_link_list_free`.
///
/// # Safety
/// `name` is a NUL-terminated string. `data` is NULL or points to `len` readable bytes.
/// `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_import_links(
    name: *const c_char,
    data: *const u8,
    len: usize,
    error: *mut *mut c_char,
) -> *mut RcGitImportLinkList {
    // SAFETY: forwarded from this function's contract.
    let (name, data, error) = unsafe { (bytes_arg(name), slice_arg(data, len), Out::new(error)) };
    let Some(name) = error.required("rcgit_import_links", "name", name) else {
        return ptr::null_mut();
    };
    let Some(data) = error.required("rcgit_import_links", "data", data) else {
        return ptr::null_mut();
    };
    let name = String::from_utf8_lossy(name);
    let Some(language) = Language::from_file_name(&name) else {
        return ptr::null_mut();
    };
    match rc_git_highlight::import_links(data, language) {
        Ok(links) => {
            let mut exported = Vec::with_capacity(links.len());
            for link in links {
                exported.push(RcGitImportLink {
                    start: link.start,
                    end: link.end,
                    specifier_start: link.specifier_start,
                    specifier_end: link.specifier_end,
                });
            }
            let (links, len) = export_vec(exported);
            export(RcGitImportLinkList { links, len })
        }
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_import_links`.
///
/// # Safety
/// `list` is NULL or came from `rcgit_import_links` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_import_link_list_free(list: *mut RcGitImportLinkList) {
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
    const OP: &str = "rcgit_guard_plan_branch_delete";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, error) =
        unsafe { (bytes_arg(repo_path), bytes_arg(name), Out::new(error)) };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return ptr::null_mut();
    };
    let Some(name) = error.required(OP, "name", name) else {
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
    const OP: &str = "rcgit_guard_branch_create";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, revspec, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(revspec),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(name) = error.required(OP, "name", name) else {
        return RCGIT_ERROR;
    };
    let Some(revspec) = error.required(OP, "revspec", revspec) else {
        return RCGIT_ERROR;
    };
    create_ref_with(create_branch, repo_path, name, revspec, &error)
}

/// What a branch switch did. Plain data, no free function.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RcGitCheckoutOutcome {
    /// The commit `HEAD` resolves to afterwards.
    pub new_head: RcGitOid,
    /// Files the working tree changed.
    pub files_changed: usize,
    /// Whether `HEAD` was already on this branch, so nothing was written.
    pub already_on_branch: bool,
}

/// Checks out the existing local branch `branch`: the working tree, the index, and then
/// `HEAD`. Refuses the whole switch when a path it touches has uncommitted work, and creates
/// nothing. Returns `RCGIT_OK` or `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` and `branch` are NUL-terminated strings; `out` is NULL or valid for writing;
/// `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_checkout(
    repo_path: *const c_char,
    branch: *const c_char,
    out: *mut RcGitCheckoutOutcome,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_checkout";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, branch, out, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(branch),
            Out::new(out),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(branch) = error.required(OP, "branch", branch) else {
        return RCGIT_ERROR;
    };
    match checkout_branch(
        Path::new(OsStr::from_bytes(repo_path)),
        &Policy::default(),
        &String::from_utf8_lossy(branch),
    ) {
        Ok(outcome) => {
            out.set(RcGitCheckoutOutcome {
                new_head: outcome.new_head.into(),
                files_changed: outcome.files_changed,
                already_on_branch: outcome.already_on_branch,
            });
            RCGIT_OK
        }
        Err(err) => {
            error.report(&err);
            RCGIT_ERROR
        }
    }
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
    const OP: &str = "rcgit_guard_tag_create";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, revspec, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(revspec),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(name) = error.required(OP, "name", name) else {
        return RCGIT_ERROR;
    };
    let Some(revspec) = error.required(OP, "revspec", revspec) else {
        return RCGIT_ERROR;
    };
    create_ref_with(create_tag, repo_path, name, revspec, &error)
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
    const OP: &str = "rcgit_guard_branch_delete";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, input, outs) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(input),
            DeletedOuts {
                backup: Out::new(backup),
                error: Out::new(error),
            },
        )
    };
    let Some(repo_path) = outs.error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(name) = outs.error.required(OP, "name", name) else {
        return RCGIT_ERROR;
    };
    delete_ref_with(delete_branch, repo_path, name, input, &outs)
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
    const OP: &str = "rcgit_guard_tag_delete";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, name, input, outs) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(name),
            bytes_arg(input),
            DeletedOuts {
                backup: Out::new(backup),
                error: Out::new(error),
            },
        )
    };
    let Some(repo_path) = outs.error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(name) = outs.error.required(OP, "name", name) else {
        return RCGIT_ERROR;
    };
    delete_ref_with(delete_tag, repo_path, name, input, &outs)
}

/// The status of a local guarded mutation (branch, tag, stash, discard), reporting the
/// message on failure.
fn finish_action(result: Result<(), ActionError>, error: &Out<*mut c_char>) -> i32 {
    match result {
        Ok(()) => RCGIT_OK,
        Err(err) => {
            error.report(&err);
            action_status(&err)
        }
    }
}

/// The status of a guarded deletion (branch, tag, stash pop or drop). On success the backup
/// ref's full name, when one was written, goes to `backup`; on failure the message goes to
/// `error`.
fn finish_deleted(
    result: ActionResult<Deleted>,
    backup: &Out<*mut c_char>,
    error: &Out<*mut c_char>,
) -> i32 {
    match result {
        Ok(deleted) => {
            if let Some(backup_ref) = deleted.backup_ref {
                backup.set(export_string(backup_ref));
            }
            RCGIT_OK
        }
        Err(err) => {
            error.report(&err);
            action_status(&err)
        }
    }
}

/// The out parameters of a guarded deletion.
struct DeletedOuts {
    backup: Out<*mut c_char>,
    error: Out<*mut c_char>,
}

/// The guard call behind `rcgit_guard_branch_create` and `rcgit_guard_tag_create`.
type CreateRef = fn(&Path, &Policy, &str, &str) -> ActionResult<()>;

/// The guard call behind `rcgit_guard_branch_delete` and `rcgit_guard_tag_delete`.
type DeleteRef = fn(&Path, &Policy, &str, Option<&str>) -> ActionResult<Deleted>;

/// The guard call behind `rcgit_guard_stash_pop` and `rcgit_guard_stash_drop`.
type RemoveStash = fn(&Path, &Policy, usize, Oid, Option<&str>) -> ActionResult<Deleted>;

/// Runs `create` on the checked arguments of a ref-creating entry point.
fn create_ref_with(
    create: CreateRef,
    repo_path: &[u8],
    name: &[u8],
    revspec: &[u8],
    error: &Out<*mut c_char>,
) -> i32 {
    finish_action(
        create(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            &String::from_utf8_lossy(name),
            &String::from_utf8_lossy(revspec),
        ),
        error,
    )
}

/// Runs `delete` on the checked arguments of a ref-deleting entry point.
fn delete_ref_with(
    delete: DeleteRef,
    repo_path: &[u8],
    name: &[u8],
    input: Option<&[u8]>,
    outs: &DeletedOuts,
) -> i32 {
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    finish_deleted(
        delete(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            &String::from_utf8_lossy(name),
            input.as_deref(),
        ),
        &outs.backup,
        &outs.error,
    )
}

/// Runs `remove` on the checked arguments of a stash-removing entry point.
fn remove_stash_with(
    remove: RemoveStash,
    repo_path: &[u8],
    entry: (usize, Oid),
    input: Option<&[u8]>,
    outs: &DeletedOuts,
) -> i32 {
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    let (index, expected) = entry;
    finish_deleted(
        remove(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            index,
            expected,
            input.as_deref(),
        ),
        &outs.backup,
        &outs.error,
    )
}

/// One hunk of a worktree diff, named by its unified header ranges. Caller-owned input.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RcGitHunkStamp {
    /// 1-based first line of the hunk on the tree side.
    pub old_start: u32,
    /// Lines the hunk covers on the tree side, context included.
    pub old_lines: u32,
    /// 1-based first line of the hunk on the disk side.
    pub new_start: u32,
    /// Lines the hunk covers on the disk side, context included.
    pub new_lines: u32,
}

/// One file of a commit selection. Caller-owned input: nothing here is freed by rc-git.
#[repr(C)]
pub struct RcGitCommitFile {
    /// Repository-relative path, NUL-terminated.
    pub path: *const c_char,
    /// NULL with `hunk_count` 0 commits the whole file; non-NULL commits only these hunks, and
    /// an empty list is refused. NULL with a nonzero count is an error.
    pub hunks: *const RcGitHunkStamp,
    /// Number of entries behind `hunks`.
    pub hunk_count: usize,
}

/// What a completed commit produced.
#[repr(C)]
pub struct RcGitCommitResult {
    /// The new commit.
    pub oid: RcGitOid,
    /// Short name of the branch that advanced.
    pub branch: *mut c_char,
}

impl Drop for RcGitCommitResult {
    fn drop(&mut self) {
        // SAFETY: `branch` was produced by `export_string` and is released once here.
        unsafe { release_string(self.branch) };
    }
}

/// The policy's decision for committing `files` selected files. Pure; release with
/// `rcgit_guard_decision_free`.
#[unsafe(no_mangle)]
pub extern "C" fn rcgit_guard_plan_commit(files: usize) -> *mut RcGitGuardDecision {
    let decision = plan_commit(&Policy::default(), files);
    export(RcGitGuardDecision::export(decision, false))
}

/// The policy's decision for stashing `files` selected files. Pure; release with
/// `rcgit_guard_decision_free`.
#[unsafe(no_mangle)]
pub extern "C" fn rcgit_guard_plan_stash_push(files: usize) -> *mut RcGitGuardDecision {
    let decision = plan_stash_push(&Policy::default(), files);
    export(RcGitGuardDecision::export(decision, false))
}

/// The policy's decision for applying a stash entry. Pure; release with
/// `rcgit_guard_decision_free`.
#[unsafe(no_mangle)]
pub extern "C" fn rcgit_guard_plan_stash_apply() -> *mut RcGitGuardDecision {
    let decision = plan_stash_apply(&Policy::default());
    export(RcGitGuardDecision::export(decision, false))
}

/// The policy's decision for popping `stash@{index}`, whose reflog message is `message`; the
/// challenge names both. Pure; NULL when `message` is NULL. Release with
/// `rcgit_guard_decision_free`.
///
/// # Safety
/// `message` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_plan_stash_pop(
    index: usize,
    message: *const c_char,
) -> *mut RcGitGuardDecision {
    // SAFETY: forwarded from this function's contract.
    let Some(message) = (unsafe { bytes_arg(message) }) else {
        return ptr::null_mut();
    };
    let decision = plan_stash_pop(&Policy::default(), index, &String::from_utf8_lossy(message));
    export(RcGitGuardDecision::export(decision, false))
}

/// The policy's decision for dropping `stash@{index}`, whose reflog message is `message`; the
/// challenge names both. Pure; NULL when `message` is NULL. Release with
/// `rcgit_guard_decision_free`.
///
/// # Safety
/// `message` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_plan_stash_drop(
    index: usize,
    message: *const c_char,
) -> *mut RcGitGuardDecision {
    // SAFETY: forwarded from this function's contract.
    let Some(message) = (unsafe { bytes_arg(message) }) else {
        return ptr::null_mut();
    };
    let decision = plan_stash_drop(&Policy::default(), index, &String::from_utf8_lossy(message));
    export(RcGitGuardDecision::export(decision, false))
}

/// The policy's decision for discarding changes in `files` files. Pure; release with
/// `rcgit_guard_decision_free`.
#[unsafe(no_mangle)]
pub extern "C" fn rcgit_guard_plan_discard(files: usize) -> *mut RcGitGuardDecision {
    let decision = plan_discard(&Policy::default(), files);
    export(RcGitGuardDecision::export(decision, false))
}

/// Reads a caller-owned array of NUL-terminated strings into owned lossy strings. `arg` names
/// the array in the error: `paths` NULL with a nonzero `len`, or `paths[2]` NULL.
///
/// # Safety
/// `ptr` is NULL (with `len` 0) or points to `len` readable, non-NULL `char *`s.
unsafe fn read_string_array(
    arg: &str,
    ptr: *const *const c_char,
    len: usize,
) -> Result<Vec<String>, BadArg> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(BadArg::new(arg, Fault::Null));
    }
    // SAFETY: forwarded from this function's contract.
    let raw = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut strings = Vec::with_capacity(len);
    for (index, item) in raw.iter().enumerate() {
        // SAFETY: forwarded from this function's contract.
        let Some(bytes) = (unsafe { bytes_arg(*item) }) else {
            return Err(BadArg::new(format!("{arg}[{index}]"), Fault::Null));
        };
        strings.push(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(strings)
}

/// Reads a caller-owned array of `RcGitCommitFile` into owned selections. Refused when the
/// array or a path is NULL, or when `hunks` is NULL with a nonzero `hunk_count`; `arg` names
/// the array in the error, as in `request.files[2].path`.
///
/// # Safety
/// `ptr` points to `len` readable `RcGitCommitFile`s whose inner pointers honor that
/// struct's contract.
unsafe fn read_selections(
    arg: &str,
    ptr: *const RcGitCommitFile,
    len: usize,
) -> Result<Vec<FileSelection>, BadArg> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(BadArg::new(arg, Fault::Null));
    }
    // SAFETY: forwarded from this function's contract.
    let raw = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut selections = Vec::with_capacity(len);
    for (index, file) in raw.iter().enumerate() {
        // SAFETY: forwarded from this function's contract.
        let Some(path_bytes) = (unsafe { bytes_arg(file.path) }) else {
            return Err(BadArg::new(format!("{arg}[{index}].path"), Fault::Null));
        };
        let path = String::from_utf8_lossy(path_bytes).into_owned();
        let stamps: &[RcGitHunkStamp] = match (file.hunks.is_null(), file.hunk_count) {
            (true, 0) => {
                selections.push(FileSelection::Whole { path });
                continue;
            }
            (true, _) => return Err(BadArg::new(format!("{arg}[{index}].hunks"), Fault::Null)),
            (false, 0) => &[],
            // SAFETY: forwarded from this function's contract.
            (false, count) => unsafe { std::slice::from_raw_parts(file.hunks, count) },
        };
        selections.push(FileSelection::Hunks {
            path,
            hunks: stamps
                .iter()
                .map(|stamp| HunkStamp {
                    old_start: stamp.old_start,
                    old_lines: stamp.old_lines,
                    new_start: stamp.new_start,
                    new_lines: stamp.new_lines,
                })
                .collect(),
        });
    }
    Ok(selections)
}

/// The fallback identity when both halves were given. It is recorded only when the
/// repository's own config sets no `user.name`/`user.email`, so the identity the app remembered
/// never overrides one the repository chose.
/// The author a commit or stash records: the caller's fallback, if any, behind the
/// repository's own config. Refused when `fallback` is set but a field of it is NULL; `arg`
/// names the struct in the error, as in `request.fallback.email`.
///
/// # Safety
/// `fallback` is NULL or points to a readable `RcGitFallbackIdentity` whose fields are NULL
/// or NUL-terminated strings.
unsafe fn fallback_author(
    arg: &str,
    fallback: *const RcGitFallbackIdentity,
) -> Result<Author, BadArg> {
    if fallback.is_null() {
        return Ok(Author {
            explicit: None,
            fallback: None,
        });
    }
    // SAFETY: forwarded from this function's contract.
    let (name, email) = unsafe {
        let fallback = &*fallback;
        (bytes_arg(fallback.name), bytes_arg(fallback.email))
    };
    let Some(name) = name else {
        return Err(BadArg::new(format!("{arg}.name"), Fault::Null));
    };
    let Some(email) = email else {
        return Err(BadArg::new(format!("{arg}.email"), Fault::Null));
    };
    Ok(Author {
        explicit: None,
        fallback: Some(Identity {
            name: String::from_utf8_lossy(name).into_owned(),
            email: String::from_utf8_lossy(email).into_owned(),
        }),
    })
}

/// The identity a commit or stash records when the repository's own config sets none.
/// Caller-owned input: nothing here is freed by rc-git.
#[repr(C)]
pub struct RcGitFallbackIdentity {
    /// `user.name`, NUL-terminated.
    pub name: *const c_char,
    /// `user.email`, NUL-terminated.
    pub email: *const c_char,
}

/// What `rcgit_guard_commit` commits. Caller-owned input: nothing here is freed by rc-git.
#[repr(C)]
pub struct RcGitCommitRequest {
    /// The commit message, NUL-terminated.
    pub message: *const c_char,
    /// Recorded only when the repository's own config sets no identity, or NULL for none.
    pub fallback: *const RcGitFallbackIdentity,
    /// The commit the change list was built against, or NULL on an unborn branch.
    pub expected_head: *const RcGitOid,
    /// The selected files.
    pub files: *const RcGitCommitFile,
    /// Number of entries behind `files`.
    pub file_count: usize,
}

/// A commit request read into owned values.
struct OwnedCommitRequest {
    message: String,
    author: Author,
    expected_head: Option<Oid>,
    selections: Vec<FileSelection>,
}

impl OwnedCommitRequest {
    /// Reads the caller's request. Refused, naming the part, when it, its message, a
    /// selection path, a counted hunk list, or a field of a set fallback is NULL, or when a
    /// set `expected_head` is malformed.
    ///
    /// # Safety
    /// `request` is NULL or points to a readable `RcGitCommitRequest` per that struct's
    /// contract.
    unsafe fn read(request: *const RcGitCommitRequest) -> Result<Self, BadArg> {
        if request.is_null() {
            return Err(BadArg::new("request", Fault::Null));
        }
        // SAFETY: forwarded from this function's contract.
        let (message, author, expected_head, selections) = unsafe {
            let request = &*request;
            (
                bytes_arg(request.message),
                fallback_author("request.fallback", request.fallback)?,
                optional_oid_arg(request.expected_head),
                read_selections("request.files", request.files, request.file_count)?,
            )
        };
        let Some(message) = message else {
            return Err(BadArg::new("request.message", Fault::Null));
        };
        let expected_head =
            expected_head.map_err(|fault| BadArg::new("request.expected_head", fault))?;
        Ok(Self {
            message: String::from_utf8_lossy(message).into_owned(),
            author,
            expected_head,
            selections,
        })
    }
}

/// What `rcgit_guard_stash_push` stashes. Caller-owned input: nothing here is freed by
/// rc-git.
#[repr(C)]
pub struct RcGitStashRequest {
    /// Repository-relative paths, each NUL-terminated and non-NULL.
    pub paths: *const *const c_char,
    /// Number of entries behind `paths`.
    pub path_count: usize,
    /// The stash message, or NULL for git's default.
    pub message: *const c_char,
    /// Recorded only when the repository's own config sets no identity, or NULL for none.
    pub fallback: *const RcGitFallbackIdentity,
}

/// A stash request read into owned values.
struct OwnedStashRequest {
    paths: Vec<String>,
    message: Option<String>,
    author: Author,
}

impl OwnedStashRequest {
    /// Reads the caller's request. Refused, naming the part, when it, a path, or a field of a
    /// set fallback is NULL.
    ///
    /// # Safety
    /// `request` is NULL or points to a readable `RcGitStashRequest` per that struct's
    /// contract.
    unsafe fn read(request: *const RcGitStashRequest) -> Result<Self, BadArg> {
        if request.is_null() {
            return Err(BadArg::new("request", Fault::Null));
        }
        // SAFETY: forwarded from this function's contract.
        let (paths, message, author) = unsafe {
            let request = &*request;
            (
                read_string_array("request.paths", request.paths, request.path_count)?,
                bytes_arg(request.message),
                fallback_author("request.fallback", request.fallback)?,
            )
        };
        Ok(Self {
            paths,
            message: message.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            author,
        })
    }
}

/// A commit identity: `user.name` and `user.email`.
#[repr(C)]
pub struct RcGitIdentity {
    /// `user.name`.
    pub name: *mut c_char,
    /// `user.email`.
    pub email: *mut c_char,
}

impl Drop for RcGitIdentity {
    fn drop(&mut self) {
        // SAFETY: both strings were produced by `export_string` and are released once here.
        unsafe {
            release_string(self.name);
            release_string(self.email);
        }
    }
}

/// The repository's own `user.name`/`user.email`, read fresh from its config: the identity a
/// commit records ahead of any fallback. Returns NULL without setting `error` when the config
/// sets either one empty or not at all. Release with `rcgit_identity_free`.
///
/// # Safety
/// `repo_path` is a NUL-terminated string. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_configured_identity(
    repo_path: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitIdentity {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, error) = unsafe { (bytes_arg(repo_path), Out::new(error)) };
    let Some(repo_path) = error.required("rcgit_guard_configured_identity", "repo_path", repo_path)
    else {
        return ptr::null_mut();
    };
    match configured_identity(Path::new(OsStr::from_bytes(repo_path))) {
        Ok(Some(identity)) => export(RcGitIdentity {
            name: export_string(identity.name.into_bytes()),
            email: export_string(identity.email.into_bytes()),
        }),
        Ok(None) => ptr::null_mut(),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a value from `rcgit_guard_configured_identity`.
///
/// # Safety
/// `identity` is NULL or came from `rcgit_guard_configured_identity` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_identity_free(identity: *mut RcGitIdentity) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(identity) };
}

/// Commits the selected files (or hunks) to the checked-out branch. The commit is refused
/// when `HEAD` moved from `request.expected_head`. `request.fallback` is recorded only when
/// the repository's own config sets no `user.name`/`user.email`; without either the commit
/// is refused with `RCGIT_IDENTITY_REQUIRED`. On success `out` holds the result; release it
/// with `rcgit_commit_result_free`. Returns `RCGIT_OK`, `RCGIT_IDENTITY_REQUIRED`, or
/// `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` is a NUL-terminated string and `input` is NULL or one. `request` is NULL or
/// points to a readable `RcGitCommitRequest` per that struct's contract. `out` is valid for
/// writing a `RcGitCommitResult *`. `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_commit(
    repo_path: *const c_char,
    request: *const RcGitCommitRequest,
    input: *const c_char,
    out: *mut *mut RcGitCommitResult,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_commit";
    let out_missing = out.is_null();
    // SAFETY: forwarded from this function's contract.
    let (repo_path, request, input, out, error) = unsafe {
        (
            bytes_arg(repo_path),
            OwnedCommitRequest::read(request),
            bytes_arg(input),
            Out::new(out),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(request) = error.read(OP, request) else {
        return RCGIT_ERROR;
    };
    if out_missing {
        error.report(bad_arg(OP, "out", &Fault::Null));
        return RCGIT_ERROR;
    }
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    match commit(
        Path::new(OsStr::from_bytes(repo_path)),
        &Policy::default(),
        CommitRequest {
            message: &request.message,
            author: request.author,
            expected_head: request.expected_head,
            selections: &request.selections,
        },
        input.as_deref(),
    ) {
        Ok(committed) => {
            out.set(export(RcGitCommitResult {
                oid: committed.oid.into(),
                branch: export_string(committed.branch.into_bytes()),
            }));
            RCGIT_OK
        }
        Err(err) => {
            error.report(&err);
            action_status(&err)
        }
    }
}

/// Releases a value from `rcgit_guard_commit`.
///
/// # Safety
/// `result` is NULL or came from `rcgit_guard_commit` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_commit_result_free(result: *mut RcGitCommitResult) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(result) };
}

/// Stashes the changes at `request.paths` as a git-compatible entry, then restores those
/// files to `HEAD` state. `request.fallback` is recorded only when the repository's own
/// config sets no identity, as for `rcgit_guard_commit`. Returns `RCGIT_OK`,
/// `RCGIT_IDENTITY_REQUIRED`, or `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` is a NUL-terminated string and `input` is NULL or one. `request` is NULL or
/// points to a readable `RcGitStashRequest` per that struct's contract. `error` is NULL or
/// valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_stash_push(
    repo_path: *const c_char,
    request: *const RcGitStashRequest,
    input: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_stash_push";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, request, input, error) = unsafe {
        (
            bytes_arg(repo_path),
            OwnedStashRequest::read(request),
            bytes_arg(input),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(request) = error.read(OP, request) else {
        return RCGIT_ERROR;
    };
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    let pushed = stash_push(
        Path::new(OsStr::from_bytes(repo_path)),
        &Policy::default(),
        StashRequest {
            paths: &request.paths,
            message: request.message.as_deref(),
            author: request.author,
        },
        input.as_deref(),
    );
    finish_action(pushed.map(|_| ()), &error)
}

/// The status code of a local guarded mutation's error (commit, stash, discard, branch, tag).
/// Exhaustive, so a new variant the app must branch on gets a status here rather than
/// reaching Swift only as text.
fn action_status(err: &ActionError) -> i32 {
    match err {
        ActionError::MissingIdentity => RCGIT_IDENTITY_REQUIRED,
        ActionError::Denied { .. }
        | ActionError::NotConfirmed { .. }
        | ActionError::CheckedOut { .. }
        | ActionError::DetachedHead { .. }
        | ActionError::StaleRepository { .. }
        | ActionError::HunkMismatch { .. }
        | ActionError::NoHunksSelected { .. }
        | ActionError::DirtyPath { .. }
        | ActionError::AuthRequired { .. }
        | ActionError::PassphraseRequired { .. }
        | ActionError::AgentSignRefused { .. }
        | ActionError::Diverged { .. }
        | ActionError::PushRejected { .. }
        | ActionError::UnknownHostKey { .. }
        | ActionError::UnsupportedRemote { .. }
        | ActionError::HostKeyMismatch { .. }
        | ActionError::NoUpstream { .. }
        | ActionError::UpstreamGone { .. }
        | ActionError::Cancelled { .. }
        | ActionError::Git { .. } => RCGIT_ERROR,
    }
}

/// Writes `stash@{index}` back to the working tree, keeping the entry. `expected` is the
/// stash commit the caller showed at that position; refused when the entry there is now
/// another one, or when any touched path has uncommitted changes. Returns `RCGIT_OK` or
/// `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` is a NUL-terminated string; `expected` points to a readable `RcGitOid`;
/// `input` is NULL or a NUL-terminated string. `error` is NULL or valid for writing a
/// `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_stash_apply(
    repo_path: *const c_char,
    index: usize,
    expected: *const RcGitOid,
    input: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_stash_apply";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, expected, input, error) = unsafe {
        (
            bytes_arg(repo_path),
            oid_arg(expected),
            bytes_arg(input),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(expected) = error.valid(OP, "expected", expected) else {
        return RCGIT_ERROR;
    };
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    finish_action(
        stash_apply(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            index,
            expected,
            input.as_deref(),
        ),
        &error,
    )
}

/// Applies `stash@{index}` and drops it once every file is restored, refusing as
/// `rcgit_guard_stash_apply` does when the entry there is no longer `expected`. On success,
/// the backup ref's full name is stored into `backup` (release with `rcgit_string_free`).
/// Returns `RCGIT_OK` or `RCGIT_ERROR`.
///
/// # Safety
/// As `rcgit_guard_stash_apply`; `backup` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_stash_pop(
    repo_path: *const c_char,
    index: usize,
    expected: *const RcGitOid,
    input: *const c_char,
    backup: *mut *mut c_char,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_stash_pop";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, expected, input, outs) = unsafe {
        (
            bytes_arg(repo_path),
            oid_arg(expected),
            bytes_arg(input),
            DeletedOuts {
                backup: Out::new(backup),
                error: Out::new(error),
            },
        )
    };
    let Some(repo_path) = outs.error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(expected) = outs.error.valid(OP, "expected", expected) else {
        return RCGIT_ERROR;
    };
    remove_stash_with(stash_pop, repo_path, (index, expected), input, &outs)
}

/// Deletes `stash@{index}` without applying it. Same contract as `rcgit_guard_stash_pop`.
///
/// # Safety
/// As `rcgit_guard_stash_pop`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_stash_drop(
    repo_path: *const c_char,
    index: usize,
    expected: *const RcGitOid,
    input: *const c_char,
    backup: *mut *mut c_char,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_stash_drop";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, expected, input, outs) = unsafe {
        (
            bytes_arg(repo_path),
            oid_arg(expected),
            bytes_arg(input),
            DeletedOuts {
                backup: Out::new(backup),
                error: Out::new(error),
            },
        )
    };
    let Some(repo_path) = outs.error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(expected) = outs.error.valid(OP, "expected", expected) else {
        return RCGIT_ERROR;
    };
    remove_stash_with(stash_drop, repo_path, (index, expected), input, &outs)
}

/// Discards every change at `paths`: tracked files return to `HEAD` bytes, untracked files
/// are deleted. Returns `RCGIT_OK` or `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` is a NUL-terminated string; `input` is NULL or one. `paths` points to `len`
/// readable, non-NULL NUL-terminated strings. `error` is NULL or valid for writing a
/// `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_discard(
    repo_path: *const c_char,
    paths: *const *const c_char,
    len: usize,
    input: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_discard";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, paths, input, error) = unsafe {
        (
            bytes_arg(repo_path),
            read_string_array("paths", paths, len),
            bytes_arg(input),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(paths) = error.read(OP, paths) else {
        return RCGIT_ERROR;
    };
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    finish_action(
        // The app refreshes the changes list afterwards, so the per-path outcomes are dropped.
        discard_files(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            &paths,
            input.as_deref(),
        )
        .map(|_| ()),
        &error,
    )
}

/// Discards only the given hunks of one file, keeping its other changes. Returns `RCGIT_OK`
/// or `RCGIT_ERROR`.
///
/// # Safety
/// `repo_path` and `path` are NUL-terminated strings; `input` is NULL or one. `stamps`
/// points to `len` readable `RcGitHunkStamp`s. `error` is NULL or valid for writing a
/// `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_discard_hunks(
    repo_path: *const c_char,
    path: *const c_char,
    stamps: *const RcGitHunkStamp,
    len: usize,
    input: *const c_char,
    error: *mut *mut c_char,
) -> i32 {
    const OP: &str = "rcgit_guard_discard_hunks";
    // SAFETY: forwarded from this function's contract.
    let (repo_path, path, stamps, input, error) = unsafe {
        (
            bytes_arg(repo_path),
            bytes_arg(path),
            match (stamps.is_null(), len) {
                (_, 0) => Err(Fault::Empty),
                (true, _) => Err(Fault::Null),
                (false, len) => Ok(std::slice::from_raw_parts(stamps, len).to_vec()),
            },
            bytes_arg(input),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required(OP, "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let Some(path) = error.required(OP, "path", path) else {
        return RCGIT_ERROR;
    };
    let Some(stamps) = error.valid(OP, "stamps", stamps) else {
        return RCGIT_ERROR;
    };
    let stamps: Vec<HunkStamp> = stamps
        .iter()
        .map(|stamp| HunkStamp {
            old_start: stamp.old_start,
            old_lines: stamp.old_lines,
            new_start: stamp.new_start,
            new_lines: stamp.new_lines,
        })
        .collect();
    let input = input.map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    finish_action(
        discard_hunks(
            Path::new(OsStr::from_bytes(repo_path)),
            &Policy::default(),
            &String::from_utf8_lossy(path),
            &stamps,
            input.as_deref(),
        ),
        &error,
    )
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
    const OP: &str = "rcgit_resolve";
    // SAFETY: forwarded from this function's contract.
    let (repo, spec, out, error) = unsafe {
        (
            repo_arg(repo),
            bytes_arg(spec),
            Out::new(out),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return RCGIT_ERROR;
    };
    let Some(spec) = error.required(OP, "spec", spec) else {
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
    const OP: &str = "rcgit_resolve_tree";
    // SAFETY: forwarded from this function's contract.
    let (repo, spec, out, error) = unsafe {
        (
            repo_arg(repo),
            bytes_arg(spec),
            Out::new(out),
            Out::new(error),
        )
    };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return RCGIT_ERROR;
    };
    let Some(spec) = error.required(OP, "spec", spec) else {
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
    const OP: &str = "rcgit_tree_of";
    // SAFETY: forwarded from this function's contract.
    let (repo, oid, out, error) =
        unsafe { (repo_arg(repo), oid_arg(oid), Out::new(out), Out::new(error)) };
    let Some(repo) = error.required(OP, "repo", repo) else {
        return RCGIT_ERROR;
    };
    let Some(oid) = error.valid(OP, "oid", oid) else {
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
    let Ok(oid) = (unsafe { oid_arg(oid) }) else {
        return ptr::null_mut();
    };
    export_string(oid.to_hex())
}

// ---------------------------------------------------------------------------------------------
// Remote operations. Network I/O runs through rc-git-guard; credentials come from the
// caller, so no credential helper is ever spawned.
// ---------------------------------------------------------------------------------------------

/// Where the checked-out branch pushes and pulls, as of the last fetch. Free with
/// `rcgit_remote_status_free`.
#[repr(C)]
pub struct RcGitRemoteStatus {
    /// Upstream remote name, e.g. `origin`. Empty when `has_upstream` is false.
    pub remote: *mut c_char,
    /// Fetch URL of that remote, userinfo removed.
    pub url: *mut c_char,
    /// Host (with port when present) parsed from the URL — the credential key.
    pub host: *mut c_char,
    /// Short name of the checked-out branch.
    pub branch: *mut c_char,
    /// Commits on the branch that the tracking ref does not have.
    pub ahead: usize,
    /// Commits on the tracking ref that the branch does not have.
    pub behind: usize,
    /// Whether a remote with a fetch URL was resolved for the branch: `branch.<name>.remote`,
    /// else `origin` when it exists. True even before a tracking ref exists; `ahead` and
    /// `behind` are then 0.
    pub has_upstream: bool,
    /// Whether the remote is ssh (`ssh://` or scp-like), so it takes a key passphrase
    /// rather than an HTTPS token. False without an upstream.
    pub uses_ssh: bool,
}

impl Drop for RcGitRemoteStatus {
    fn drop(&mut self) {
        // SAFETY: every string was produced by `export_string` and is released once here.
        unsafe {
            release_string(self.remote);
            release_string(self.url);
            release_string(self.host);
            release_string(self.branch);
        }
    }
}

/// What a fetch changed. Plain data, no free function.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RcGitFetchOutcome {
    /// Remote-tracking refs created or moved.
    pub updated_refs: usize,
}

/// What a fast-forward pull did. Plain data, no free function.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RcGitPullOutcome {
    /// The commit the branch now points at.
    pub new_head: RcGitOid,
    /// Files the working tree changed.
    pub files_changed: usize,
    /// Whether there was nothing to pull.
    pub already_up_to_date: bool,
}

/// What a push did. Plain data, no free function.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RcGitPushOutcome {
    /// Commits uploaded.
    pub pushed_commits: usize,
    /// Whether there was nothing to push.
    pub already_up_to_date: bool,
}

/// Maps a remote-action error to its status code and reports the message. An unknown
/// host key additionally fills `hostkey` with `"<key type> <fingerprint>"`; a locked ssh
/// key fills it with the key file's path.
fn finish_remote(error: &Out<*mut c_char>, hostkey: &Out<*mut c_char>, err: &ActionError) -> i32 {
    error.report(err);
    match err {
        ActionError::AuthRequired { .. } => RCGIT_AUTH_REQUIRED,
        ActionError::PassphraseRequired { key, .. } => {
            hostkey.set(export_string(key.clone()));
            RCGIT_PASSPHRASE_REQUIRED
        }
        ActionError::Diverged { .. } => RCGIT_DIVERGED,
        ActionError::UnknownHostKey {
            key_type,
            fingerprint,
            ..
        } => {
            hostkey.set(export_string(format!("{key_type} {fingerprint}")));
            RCGIT_HOSTKEY_UNKNOWN
        }
        ActionError::Denied { .. }
        | ActionError::NotConfirmed { .. }
        | ActionError::CheckedOut { .. }
        | ActionError::DetachedHead { .. }
        | ActionError::StaleRepository { .. }
        | ActionError::HunkMismatch { .. }
        | ActionError::NoHunksSelected { .. }
        | ActionError::DirtyPath { .. }
        | ActionError::UnsupportedRemote { .. }
        | ActionError::HostKeyMismatch { .. }
        | ActionError::AgentSignRefused { .. }
        | ActionError::PushRejected { .. }
        | ActionError::NoUpstream { .. }
        | ActionError::UpstreamGone { .. }
        | ActionError::Git { .. } => RCGIT_ERROR,
        ActionError::Cancelled { .. } => RCGIT_CANCELLED,
        ActionError::MissingIdentity => RCGIT_IDENTITY_REQUIRED,
    }
}

/// Authentication inputs for the remote functions. Every field may be NULL: `username`
/// and `token` for anonymous HTTPS (over ssh, `token` is the key passphrase and
/// `username` is ignored), `host` to offer them to the host the remote URL names,
/// `accepted_hostkey` when no fingerprint has been approved, and `ssh_dir` to use `~/.ssh`.
#[repr(C)]
pub struct RcGitRemoteAuth {
    /// HTTPS username, or NULL (empty when `token` is set).
    pub username: *const c_char,
    /// HTTPS token, or the ssh key passphrase, or NULL.
    pub token: *const c_char,
    /// The host (`host` or `host:port`) the credentials were issued for: an HTTPS 401 from
    /// any other host gets none. NULL for the host the remote URL names.
    pub host: *const c_char,
    /// An approved `SHA256:...` host-key fingerprint, or NULL.
    pub accepted_hostkey: *const c_char,
    /// Directory holding ssh `config`/`known_hosts`/keys, or NULL for `~/.ssh`.
    pub ssh_dir: *const c_char,
}

/// Owned mirror of [`RcGitRemoteAuth`], so the borrowed [`RemoteAuth`] the guard wants
/// can be built from stable storage.
#[derive(Default)]
struct OwnedRemoteAuth {
    creds: Option<Credentials>,
    host: Option<String>,
    accepted_hostkey: Option<String>,
    ssh_dir: Option<std::path::PathBuf>,
}

impl OwnedRemoteAuth {
    /// Reads the caller's struct.
    ///
    /// # Safety
    /// `auth` is NULL or valid, and its non-NULL fields are NUL-terminated strings.
    unsafe fn read(auth: *const RcGitRemoteAuth) -> Self {
        if auth.is_null() {
            return Self::default();
        }
        // SAFETY: forwarded from this function's contract.
        let (creds, host, accepted_hostkey, ssh_dir) = unsafe {
            let auth = &*auth;
            (
                credentials_from(auth.username, auth.token),
                bytes_arg(auth.host).map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                bytes_arg(auth.accepted_hostkey)
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                bytes_arg(auth.ssh_dir)
                    .map(|bytes| std::path::PathBuf::from(OsStr::from_bytes(bytes))),
            )
        };
        Self {
            creds,
            host,
            accepted_hostkey,
            ssh_dir,
        }
    }

    fn borrow(&self) -> RemoteAuth<'_> {
        RemoteAuth {
            creds: self.creds.as_ref(),
            host: self.host.as_deref(),
            accepted_hostkey: self.accepted_hostkey.as_deref(),
            ssh_dir: self.ssh_dir.as_deref(),
        }
    }
}

/// The caller's credentials, when a token was given. A NULL username becomes empty, so an
/// ssh passphrase passed without one still reaches the key.
///
/// # Safety
/// Both pointers are NULL or NUL-terminated strings.
unsafe fn credentials_from(username: *const c_char, token: *const c_char) -> Option<Credentials> {
    // SAFETY: forwarded from this function's contract.
    let (username, token) = unsafe { (bytes_arg(username), bytes_arg(token)) };
    let token = token?;
    Some(Credentials {
        username: username
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default(),
        token: String::from_utf8_lossy(token).into_owned(),
    })
}

/// The checked-out branch's upstream and ahead/behind counts. Local only, no network.
/// Returns NULL on failure, with the message in `error`.
///
/// # Safety
/// `repo_path` is a NUL-terminated path; `error` is NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_remote_preflight(
    repo_path: *const c_char,
    error: *mut *mut c_char,
) -> *mut RcGitRemoteStatus {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, error) = unsafe { (bytes_arg(repo_path), Out::new(error)) };
    let Some(repo_path) = error.required("rcgit_guard_remote_preflight", "repo_path", repo_path)
    else {
        return ptr::null_mut();
    };
    match upstream_status(Path::new(OsStr::from_bytes(repo_path))) {
        Ok(status) => export(RcGitRemoteStatus {
            remote: export_string(status.remote),
            url: export_string(status.url),
            host: export_string(status.host),
            branch: export_string(status.branch),
            ahead: status.ahead,
            behind: status.behind,
            has_upstream: status.has_upstream,
            uses_ssh: status.uses_ssh,
        }),
        Err(err) => {
            error.report(err);
            ptr::null_mut()
        }
    }
}

/// Releases a status returned by `rcgit_guard_remote_preflight`.
///
/// # Safety
/// `status` is NULL or came from `rcgit_guard_remote_preflight` and is not freed twice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_remote_status_free(status: *mut RcGitRemoteStatus) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(status) }
}

/// A cancellation flag for one remote operation or one highlight. Opaque: create with
/// `rcgit_cancel_new`, pass it to `rcgit_guard_fetch`, `rcgit_guard_push` or
/// `rcgit_highlight`, set it from any thread with `rcgit_cancel_signal`, release it with
/// `rcgit_cancel_free` once the operation has returned.
pub struct RcGitCancel {
    remote: Cancel,
    /// The same signal in the shape tree-sitter polls, which is a nonzero `AtomicUsize`.
    highlight: AtomicUsize,
}

const _: () = assert_send_sync::<RcGitCancel>();

/// A fresh flag, nobody having cancelled yet. Release with `rcgit_cancel_free`.
#[unsafe(no_mangle)]
pub extern "C" fn rcgit_cancel_new() -> *mut RcGitCancel {
    export(RcGitCancel {
        remote: Cancel::new(),
        highlight: AtomicUsize::new(0),
    })
}

/// Asks the operation holding `cancel` to stop. Safe to call from a thread other than
/// the one inside the operation, which is the point: that thread is blocked in it.
/// The operation then fails with `RCGIT_CANCELLED`. NULL is a no-op.
///
/// # Safety
/// `cancel` is NULL or came from `rcgit_cancel_new` and has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_cancel_signal(cancel: *const RcGitCancel) {
    // SAFETY: forwarded from this function's contract.
    if let Some(cancel) = unsafe { cancel_flags(cancel) } {
        cancel.remote.signal();
        cancel.highlight.store(1, Ordering::Relaxed);
    }
}

/// Releases a flag. Free it only after the operation it was passed to has returned.
///
/// # Safety
/// `cancel` is NULL or came from `rcgit_cancel_new`, is not freed twice, and is not
/// in use by a running `rcgit_guard_fetch`, `rcgit_guard_push` or `rcgit_highlight`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_cancel_free(cancel: *mut RcGitCancel) {
    // SAFETY: forwarded from this function's contract.
    unsafe { release(cancel) };
}

/// # Safety
/// `cancel` is NULL or a pointer from `rcgit_cancel_new` that has not been freed.
unsafe fn cancel_flags<'a>(cancel: *const RcGitCancel) -> Option<&'a RcGitCancel> {
    if cancel.is_null() {
        return None;
    }
    // SAFETY: forwarded from this function's contract.
    Some(unsafe { &*cancel })
}

/// # Safety
/// `cancel` is NULL or a pointer from `rcgit_cancel_new` that has not been freed.
unsafe fn cancel_arg<'a>(cancel: *const RcGitCancel) -> Option<&'a Cancel> {
    // SAFETY: forwarded from this function's contract.
    unsafe { cancel_flags(cancel) }.map(|cancel| &cancel.remote)
}

/// Fetches from the branch's upstream remote, updating remote-tracking refs. `auth`
/// may be NULL for an anonymous attempt with default ssh configuration; `cancel` may
/// be NULL when the caller has no way to cancel. Returns `RCGIT_OK`, `RCGIT_ERROR`,
/// `RCGIT_AUTH_REQUIRED`, `RCGIT_CANCELLED`, `RCGIT_HOSTKEY_UNKNOWN` (`hostkey` then
/// holds the offered key for the user to verify), or `RCGIT_PASSPHRASE_REQUIRED`
/// (`hostkey` then holds the locked key file's path).
///
/// # Safety
/// `repo_path` is a NUL-terminated path; `auth` is NULL or valid with NULL or
/// NUL-terminated fields; `cancel` is NULL or came from `rcgit_cancel_new` and is not
/// freed before this returns; `out` is NULL or valid for writing; `hostkey` and
/// `error` are each NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_fetch(
    repo_path: *const c_char,
    auth: *const RcGitRemoteAuth,
    cancel: *const RcGitCancel,
    out: *mut RcGitFetchOutcome,
    hostkey: *mut *mut c_char,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, auth, cancel, out, hostkey, error) = unsafe {
        (
            bytes_arg(repo_path),
            OwnedRemoteAuth::read(auth),
            cancel_arg(cancel),
            Out::new(out),
            Out::new(hostkey),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required("rcgit_guard_fetch", "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let repo_path = Path::new(OsStr::from_bytes(repo_path));
    let never = Cancel::new();
    let cancel = cancel.unwrap_or(&never);
    match fetch_upstream(repo_path, &Policy::default(), auth.borrow(), cancel) {
        Ok(outcome) => {
            out.set(RcGitFetchOutcome {
                updated_refs: outcome.updated_refs.len(),
            });
            RCGIT_OK
        }
        Err(err) => finish_remote(&error, &hostkey, &err),
    }
}

/// Fast-forwards the checked-out branch to its tracking ref. No network; fetch first.
/// Returns `RCGIT_OK`, `RCGIT_ERROR`, or `RCGIT_DIVERGED`.
///
/// # Safety
/// `repo_path` is a NUL-terminated path; `out` is NULL or valid for writing; `error` is
/// NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_pull_ff(
    repo_path: *const c_char,
    out: *mut RcGitPullOutcome,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract; the null Out is trivially valid.
    let (repo_path, out, no_hostkey, error) = unsafe {
        (
            bytes_arg(repo_path),
            Out::new(out),
            Out::new(ptr::null_mut()),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required("rcgit_guard_pull_ff", "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    match pull_fast_forward(Path::new(OsStr::from_bytes(repo_path)), &Policy::default()) {
        Ok(outcome) => {
            out.set(RcGitPullOutcome {
                new_head: outcome.new.into(),
                files_changed: outcome.files_changed,
                already_up_to_date: outcome.already_up_to_date,
            });
            RCGIT_OK
        }
        // Pull is local-only; it can never produce a host-key report.
        Err(err) => finish_remote(&error, &no_hostkey, &err),
    }
}

/// Pushes the checked-out branch to its upstream. Plain push only; a force push is not
/// offered here. `cancel` may be NULL when the caller has no way to cancel. Returns
/// `RCGIT_OK`, `RCGIT_ERROR` (also when the remote has commits the branch lacks: the
/// message says to fetch and pull first), `RCGIT_AUTH_REQUIRED`, `RCGIT_CANCELLED`,
/// `RCGIT_HOSTKEY_UNKNOWN` (`hostkey` then holds the offered key), or
/// `RCGIT_PASSPHRASE_REQUIRED` (`hostkey` then holds the locked key file's path).
///
/// # Safety
/// `repo_path` is a NUL-terminated path; `auth` is NULL or valid with NULL or
/// NUL-terminated fields; `cancel` is NULL or came from `rcgit_cancel_new` and is not
/// freed before this returns; `out` is NULL or valid for writing; `hostkey` and
/// `error` are each NULL or valid for writing a `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rcgit_guard_push(
    repo_path: *const c_char,
    auth: *const RcGitRemoteAuth,
    cancel: *const RcGitCancel,
    out: *mut RcGitPushOutcome,
    hostkey: *mut *mut c_char,
    error: *mut *mut c_char,
) -> i32 {
    // SAFETY: forwarded from this function's contract.
    let (repo_path, auth, cancel, out, hostkey, error) = unsafe {
        (
            bytes_arg(repo_path),
            OwnedRemoteAuth::read(auth),
            cancel_arg(cancel),
            Out::new(out),
            Out::new(hostkey),
            Out::new(error),
        )
    };
    let Some(repo_path) = error.required("rcgit_guard_push", "repo_path", repo_path) else {
        return RCGIT_ERROR;
    };
    let never = Cancel::new();
    let cancel = cancel.unwrap_or(&never);
    match push_upstream(
        Path::new(OsStr::from_bytes(repo_path)),
        &Policy::default(),
        auth.borrow(),
        cancel,
    ) {
        Ok(outcome) => {
            out.set(RcGitPushOutcome {
                pushed_commits: outcome.pushed_commits,
                already_up_to_date: outcome.already_up_to_date,
            });
            RCGIT_OK
        }
        Err(err) => finish_remote(&error, &hostkey, &err),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::undocumented_unsafe_blocks,
        reason = "tests exercise the C contract directly"
    )]

    use std::collections::BTreeSet;
    use std::ffi::CString;

    use rc_git_testkit::{BIN_DAT, COMMIT_1_TITLE, COMMIT_2_TITLE, Fixture, MAIN_RS, README};

    use super::*;

    /// A refused agent signature is not a credential the app can prompt for: it reports
    /// the message as a plain error, never `RCGIT_AUTH_REQUIRED`.
    #[test]
    fn a_refused_agent_signature_is_a_non_prompting_remote_error() {
        let err = ActionError::AgentSignRefused {
            op: "fetch",
            host: "github.com".to_owned(),
            key: "'work@laptop' (SHA256:abc)".to_owned(),
        };
        let mut message = ptr::null_mut();
        let mut hostkey = ptr::null_mut();
        let (error, hostkey_out) =
            unsafe { (Out::new(&raw mut message), Out::new(&raw mut hostkey)) };
        assert_eq!(finish_remote(&error, &hostkey_out, &err), RCGIT_ERROR);
        assert!(hostkey.is_null());
        assert!(take_error(&mut message).contains("refused to sign with 'work@laptop'"));
    }

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

    /// The value behind a fallible call's result, failing the test with the call's error
    /// rather than dereferencing NULL, which would abort the whole test binary.
    fn deref<'a, T>(ptr: *const T, error: &mut *mut c_char) -> &'a T {
        assert!(!ptr.is_null(), "{}", take_error(error));
        unsafe { &*ptr }
    }

    /// Each line's bytes, cut from the hunk's buffer by consecutive starts.
    fn hunk_line_texts(hunk: &RcGitDiffHunk) -> Vec<&[u8]> {
        let data = unsafe { std::slice::from_raw_parts(hunk.data, hunk.data_len) };
        let lines = unsafe { std::slice::from_raw_parts(hunk.lines, hunk.line_count) };
        let mut texts = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let end = lines
                .get(index + 1)
                .map_or(data.len(), |next| next.start as usize);
            texts.push(&data[line.start as usize..end]);
        }
        texts
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
        let mut out = RcGitOid::empty();
        let mut error = ptr::null_mut();
        let status = unsafe { rcgit_resolve(repo, spec.as_ptr(), &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        out
    }

    fn hex(oid: &RcGitOid) -> String {
        take_string(unsafe { rcgit_oid_hex(oid) })
    }

    fn commit_id(hex: &str) -> RcGitOid {
        Oid::from_hex(hex).unwrap().into()
    }

    #[test]
    fn version_is_static_and_matches_cargo() {
        assert_eq!(text(rcgit_version()), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn open_reports_null_and_bad_paths() {
        let mut error = ptr::null_mut();
        assert!(unsafe { rcgit_repo_open(ptr::null(), &raw mut error) }.is_null());
        assert_eq!(take_error(&mut error), "rcgit_repo_open: path is NULL");

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
        let work_dir = take_string(unsafe { rcgit_repo_work_dir(repo) });
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(canonical(Path::new(&work_dir)), canonical(fx.path()));
        assert!(unsafe { rcgit_repo_git_dir(ptr::null()) }.is_null());
        assert!(unsafe { rcgit_repo_work_dir(ptr::null()) }.is_null());
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn common_dir_names_the_shared_git_dir_of_a_linked_worktree() {
        let fx = Fixture::standard();
        let main = open(&fx);
        let main_git_dir = take_string(unsafe { rcgit_repo_git_dir(main) });
        assert_eq!(
            take_string(unsafe { rcgit_repo_common_dir(main) }),
            main_git_dir
        );
        unsafe { rcgit_repo_free(main) };

        let outside = Fixture::empty_dir();
        let linked = outside.path().join("linked");
        fx.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            linked.to_str().unwrap(),
        ]);
        let path = CString::new(linked.as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();
        let repo = unsafe { rcgit_repo_open(path.as_ptr(), &raw mut error) };
        assert!(!repo.is_null() && error.is_null());
        let common = take_string(unsafe { rcgit_repo_common_dir(repo) });
        let git_dir = take_string(unsafe { rcgit_repo_git_dir(repo) });
        let canonical = |p: &str| std::fs::canonicalize(p).unwrap();
        assert_eq!(canonical(&common), canonical(&main_git_dir));
        assert_ne!(canonical(&git_dir), canonical(&common));
        assert!(unsafe { rcgit_repo_common_dir(ptr::null()) }.is_null());
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
        assert_eq!(take_error(&mut error), "rcgit_head: repo is NULL");
        unsafe { rcgit_repo_free(repo) };

        fx.git(&["checkout", "-q", "--detach", "HEAD"]);
        let repo = open(&fx);
        let head = unsafe { rcgit_head(repo, &raw mut error) };
        let head_ref = deref(head, &mut error);
        assert!(
            head_ref.ref_name.is_null(),
            "a detached HEAD names no branch"
        );
        assert!(head_ref.has_target);
        assert_eq!(hex(&head_ref.target), fx.rev_parse("HEAD"));
        unsafe { rcgit_head_free(head) };
        unsafe { rcgit_repo_free(repo) };

        let unborn = Fixture::empty_dir();
        unborn.git(&["init", "-q", "-b", "work"]);
        let repo = open(&unborn);
        let head = unsafe { rcgit_head(repo, &raw mut error) };
        let head_ref = deref(head, &mut error);
        assert!(!head_ref.has_target, "an unborn branch has no commit");
        assert_eq!(text(head_ref.ref_name), "refs/heads/work");
        unsafe { rcgit_head_free(head) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn a_bare_repository_has_no_work_dir() {
        let fx = Fixture::standard();
        let (_bare, bare_path) = fx.bare_copy();
        let path = CString::new(bare_path.as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();
        let repo = unsafe { rcgit_repo_open(path.as_ptr(), &raw mut error) };
        assert!(!repo.is_null(), "{}", take_error(&mut error));
        assert!(unsafe { rcgit_repo_work_dir(repo) }.is_null());
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
    fn a_broken_ref_is_exported_with_its_name_and_error() {
        let fx = Fixture::standard();
        fx.write(
            ".git/refs/heads/broken",
            b"0000000000000000000000000000000000000001\n",
        );
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let list = unsafe { rcgit_refs(repo, &raw mut error) };
        let list_ref = deref(list, &mut error);
        assert_eq!(list_ref.len, 5, "the broken ref hides none of the rest");
        assert_eq!(list_ref.broken_len, 1);
        let broken = unsafe { std::slice::from_raw_parts(list_ref.broken, list_ref.broken_len) };
        assert_eq!(text(broken[0].full_name), "refs/heads/broken");
        assert_ne!(text(broken[0].error), "");
        assert_ne!(text(broken[0].error), "refs/heads/broken");
        unsafe { rcgit_ref_list_free(list) };
        unsafe { rcgit_repo_free(repo) };

        assert!(unsafe { rcgit_refs(ptr::null(), &raw mut error) }.is_null());
        assert_eq!(take_error(&mut error), "rcgit_refs: repo is NULL");
    }

    #[test]
    fn export_string_strips_interior_nuls() {
        assert_eq!(take_string(export_string(b"a\0b".to_vec())), "ab");
        let empty = export_string(b"\0\0".to_vec());
        assert!(!empty.is_null(), "an all-NUL string is empty, not NULL");
        assert_eq!(take_string(empty), "");
    }

    #[test]
    fn a_non_utf8_tree_entry_name_crosses_byte_exact() {
        let fx = Fixture::standard();
        let blob = fx.hash_blob(b"raw\n");
        let mut listing = format!("100644 blob {blob}\t").into_bytes();
        listing.extend_from_slice(b"f\xffo\n");
        let tree = fx.mktree(&listing);
        let repo = open(&fx);
        let tree = resolve(repo, &tree);
        let mut error = ptr::null_mut();
        let list = unsafe { rcgit_tree_entries(repo, &raw const tree, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        let entries = unsafe { std::slice::from_raw_parts(list_ref.entries, list_ref.len) };
        assert_eq!(entries.len(), 1);
        let name = unsafe { CStr::from_ptr(entries[0].name) }.to_bytes();
        assert_eq!(name, b"f\xffo", "no U+FFFD repair on the way out");
        assert_eq!(hex(&entries[0].oid), blob);
        unsafe { rcgit_tree_list_free(list) };
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
        assert_eq!(take_error(&mut error), "rcgit_entry_at_path: root is NULL");

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
        assert!(blob_ref.valid_utf8);
        unsafe { rcgit_blob_free(blob) };

        let blob = unsafe { rcgit_blob_read(repo, &raw const main_rs, u64::MAX, &raw mut error) };
        assert_eq!(deref(blob, &mut error).limit, u64::from(u32::MAX));
        unsafe { rcgit_blob_free(blob) };

        let bin = resolve(repo, "main:bin.dat");
        let blob = unsafe { rcgit_blob_read(repo, &raw const bin, 1 << 20, &raw mut error) };
        let blob_ref = deref(blob, &mut error);
        assert_eq!(blob_ref.kind, RcGitBlobKind::Binary);
        assert_eq!(blob_ref.size, BIN_DAT.len() as u64);
        assert!(blob_ref.data.is_null());
        unsafe { rcgit_blob_free(blob) };

        let readme = resolve(repo, "main:README.md");
        let blob = unsafe { rcgit_blob_read(repo, &raw const readme, 3, &raw mut error) };
        let blob_ref = deref(blob, &mut error);
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

        let blob = unsafe { rcgit_blob_read(repo, ptr::null(), 1 << 20, &raw mut error) };
        assert!(blob.is_null());
        assert_eq!(take_error(&mut error), "rcgit_blob_read: oid is NULL");
        let malformed = RcGitOid {
            bytes: [0; 32],
            len: 33,
        };
        let blob = unsafe { rcgit_blob_read(repo, &raw const malformed, 1 << 20, &raw mut error) };
        assert!(blob.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_blob_read: oid is malformed (len 33)"
        );
        unsafe { rcgit_repo_free(repo) };
    }

    /// One page of `limit` commits from a fresh cursor at `tip`, after skipping `skip` pages
    /// of one commit each.
    fn log_page(
        repo: *const RcGitRepo,
        tip: &RcGitOid,
        skip: usize,
        limit: usize,
        error: &mut *mut c_char,
    ) -> *mut RcGitCommitList {
        let cursor = unsafe { rcgit_log_cursor_new(repo, tip, error) };
        assert!(!cursor.is_null(), "{}", take_error(error));
        for _ in 0..skip {
            let page = unsafe { rcgit_log_cursor_next(repo, cursor, 1, error) };
            assert!(!page.is_null(), "{}", take_error(error));
            unsafe { rcgit_commit_list_free(page) };
        }
        let page = unsafe { rcgit_log_cursor_next(repo, cursor, limit, error) };
        unsafe { rcgit_log_cursor_free(cursor) };
        page
    }

    #[test]
    fn log_and_single_commit() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");
        let list = log_page(repo, &tip, 0, 10, &mut error);
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

        let page = log_page(repo, &tip, 1, 10, &mut error);
        assert_eq!(
            deref(page, &mut error).len,
            1,
            "the cursor resumes after one"
        );
        unsafe { rcgit_commit_list_free(page) };
        unsafe { rcgit_repo_free(repo) };

        fx.git_with_env(
            &["commit", "-q", "--allow-empty", "-m", "zoned"],
            &[
                ("GIT_AUTHOR_DATE", "2024-01-05T00:00:00+0530"),
                ("GIT_COMMITTER_DATE", "2024-01-05T00:00:00-0800"),
            ],
        );
        let repo = open(&fx);
        let zoned = resolve(repo, "main");
        let list = log_page(repo, &zoned, 0, 1, &mut error);
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let commits = unsafe { std::slice::from_raw_parts((*list).commits, (*list).len) };
        assert_eq!(commits[0].author_offset, 19_800);
        unsafe { rcgit_commit_list_free(list) };
        let detail = unsafe { rcgit_commit_details(repo, &raw const zoned, &raw mut error) };
        assert!(!detail.is_null(), "{}", take_error(&mut error));
        assert_eq!(unsafe { &*detail }.summary.author_offset, 19_800);
        assert_eq!(unsafe { &*detail }.committer_offset, -28_800);
        unsafe { rcgit_commit_detail_free(detail) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn log_cursor_refuses_null_and_malformed_arguments() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");
        let malformed = RcGitOid {
            bytes: [0; 32],
            len: 33,
        };
        let cursor = unsafe { rcgit_log_cursor_new(repo, &raw const malformed, &raw mut error) };
        assert!(cursor.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_log_cursor_new: tip is malformed (len 33)"
        );
        let cursor = unsafe { rcgit_log_cursor_new(repo, &raw const tip, &raw mut error) };
        assert!(!cursor.is_null(), "{}", take_error(&mut error));
        let page = unsafe { rcgit_log_cursor_next(repo, ptr::null(), 10, &raw mut error) };
        assert!(page.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_log_cursor_next: cursor is NULL"
        );
        let page = unsafe { rcgit_log_cursor_next(ptr::null(), cursor, 10, &raw mut error) };
        assert!(page.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_log_cursor_next: repo is NULL"
        );
        let page = unsafe { rcgit_log_cursor_next(repo, cursor, 10, &raw mut error) };
        assert_eq!(
            deref(page, &mut error).len,
            2,
            "a refused call leaves the cursor in place"
        );
        unsafe { rcgit_commit_list_free(page) };
        unsafe { rcgit_log_cursor_free(cursor) };
        unsafe { rcgit_repo_free(repo) };
    }

    /// A filter on message and author substrings with no time bounds.
    fn text_filter(message: *const c_char, author: *const c_char) -> RcGitCommitFilter {
        RcGitCommitFilter {
            message,
            author,
            has_after: false,
            after: 0,
            has_before: false,
            before: 0,
            path: ptr::null(),
        }
    }

    /// The commits `page` holds, with its `scanned` and `complete`.
    fn page_parts(
        page: *mut RcGitCommitPage,
        error: &mut *mut c_char,
    ) -> (Vec<String>, usize, bool) {
        let page_ref = deref(page, error);
        let commits = unsafe { std::slice::from_raw_parts(page_ref.commits, page_ref.len) };
        let titles = commits.iter().map(|commit| text(commit.title)).collect();
        let parts = (titles, page_ref.scanned, page_ref.complete);
        unsafe { rcgit_commit_page_free(page) };
        parts
    }

    /// The first page of a fresh walk at `tip` over `filter`.
    fn first_page(
        repo: *const RcGitRepo,
        tip: &RcGitOid,
        filter: RcGitCommitFilter,
        (limit, max_scan): (usize, usize),
        error: &mut *mut c_char,
    ) -> *mut RcGitCommitPage {
        let walk = unsafe { rcgit_history_walk_new(repo, tip, filter, error) };
        assert!(!walk.is_null(), "{}", take_error(error));
        let page = unsafe { rcgit_history_walk_next(repo, walk, limit, max_scan, error) };
        unsafe { rcgit_history_walk_free(walk) };
        page
    }

    #[test]
    fn path_history_pages_with_a_scan_cursor() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tip = resolve(repo, "main");
        let file = CString::new("src/lib.rs").unwrap();
        let by_path = |path: &CString| RcGitCommitFilter {
            path: path.as_ptr(),
            ..text_filter(ptr::null(), ptr::null())
        };

        let page = first_page(repo, &tip, by_path(&file), (10, 100), &mut error);
        assert_eq!(
            page_parts(page, &mut error),
            (vec![COMMIT_2_TITLE.to_owned()], 2, true)
        );
        let capped = first_page(repo, &tip, by_path(&file), (10, 1), &mut error);
        let (_, scanned, complete) = page_parts(capped, &mut error);
        assert_eq!((scanned, complete), (1, false));

        let readme = CString::new("README.md").unwrap();
        let walk = unsafe {
            rcgit_history_walk_new(repo, &raw const tip, by_path(&readme), &raw mut error)
        };
        assert!(!walk.is_null(), "{}", take_error(&mut error));
        let first = unsafe { rcgit_history_walk_next(repo, walk, 10, 1, &raw mut error) };
        assert_eq!(page_parts(first, &mut error), (vec![], 1, false));
        let rest = unsafe { rcgit_history_walk_next(repo, walk, 10, 1, &raw mut error) };
        assert_eq!(
            page_parts(rest, &mut error),
            (vec![COMMIT_1_TITLE.to_owned()], 1, true),
            "the second page resumes after the commit the first examined"
        );
        let null = unsafe { rcgit_history_walk_next(repo, ptr::null(), 10, 1, &raw mut error) };
        assert!(null.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_history_walk_next: walk is NULL"
        );
        unsafe { rcgit_history_walk_free(walk) };
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

        let page = first_page(
            repo,
            &tip,
            text_filter(needle.as_ptr(), ptr::null()),
            (10, 100),
            &mut error,
        );
        assert_eq!(
            page_parts(page, &mut error),
            (vec![COMMIT_2_TITLE.to_owned()], 2, true)
        );

        let all = first_page(
            repo,
            &tip,
            text_filter(ptr::null(), ptr::null()),
            (10, 100),
            &mut error,
        );
        assert_eq!(
            page_parts(all, &mut error).0.len(),
            2,
            "NULL filters match everything"
        );

        for (path, expected) in [("src/lib.rs", 1), ("README.md", 0)] {
            let path = CString::new(path).unwrap();
            let scoped = RcGitCommitFilter {
                path: path.as_ptr(),
                ..text_filter(needle.as_ptr(), ptr::null())
            };
            let page = first_page(repo, &tip, scoped, (10, 100), &mut error);
            assert_eq!(
                page_parts(page, &mut error).0.len(),
                expected,
                "{path:?} scopes the search"
            );
        }

        let before_epoch = RcGitCommitFilter {
            has_before: true,
            ..text_filter(ptr::null(), ptr::null())
        };
        let none = first_page(repo, &tip, before_epoch, (10, 100), &mut error);
        assert!(
            page_parts(none, &mut error).0.is_empty(),
            "a set bound applies even at 0"
        );

        let initial = CString::new("Initial").unwrap();
        let filter = text_filter(initial.as_ptr(), ptr::null());
        let walk = unsafe { rcgit_history_walk_new(repo, &raw const tip, filter, &raw mut error) };
        assert!(!walk.is_null(), "{}", take_error(&mut error));
        let first = unsafe { rcgit_history_walk_next(repo, walk, 10, 1, &raw mut error) };
        assert_eq!(page_parts(first, &mut error), (vec![], 1, false));
        let rest = unsafe { rcgit_history_walk_next(repo, walk, 10, 1, &raw mut error) };
        assert_eq!(
            page_parts(rest, &mut error),
            (vec![COMMIT_1_TITLE.to_owned()], 1, true),
            "the second page resumes after the commit the first examined"
        );
        unsafe { rcgit_history_walk_free(walk) };

        let null = unsafe {
            rcgit_history_walk_new(
                ptr::null(),
                &raw const tip,
                text_filter(ptr::null(), ptr::null()),
                &raw mut error,
            )
        };
        assert!(null.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_history_walk_new: repo is NULL"
        );
        let null = unsafe {
            rcgit_history_walk_new(
                repo,
                ptr::null(),
                text_filter(ptr::null(), ptr::null()),
                &raw mut error,
            )
        };
        assert!(null.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_history_walk_new: tip is NULL"
        );
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

        let wrong = unsafe { rcgit_commit_details(repo, &raw const tree, &raw mut error) };
        assert!(wrong.is_null());
        assert!(take_error(&mut error).contains("is a tree, expected a commit"));

        let null = unsafe { rcgit_commit_details(ptr::null(), &raw const tip, &raw mut error) };
        assert!(null.is_null());
        assert_ne!(take_error(&mut error).len(), 0);
        unsafe { rcgit_commit_detail_free(ptr::null_mut()) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn resolve_variants_and_failures() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        assert_eq!(hex(&resolve(repo, "v1.0")), fx.rev_parse("v1.0"));

        let spec = CString::new("v1.0").unwrap();
        let mut out = RcGitOid::empty();
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
        assert_eq!(take_error(&mut error), "rcgit_resolve: repo is NULL");

        let malformed = RcGitOid {
            bytes: [0; 32],
            len: 33,
        };
        let status =
            unsafe { rcgit_tree_of(repo, &raw const malformed, &raw mut out, &raw mut error) };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_tree_of: oid is malformed (len 33)"
        );
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn core_reads_report_a_null_repo() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let tip = resolve(repo, "main");
        let tree = resolve(repo, "main^{tree}");
        let blob = resolve(repo, "main:README.md");
        unsafe { rcgit_repo_free(repo) };
        let path = CString::new("README.md").unwrap();
        let spec = CString::new("main").unwrap();
        let mut out = RcGitOid::empty();
        let mut size = 0;
        let mut error = ptr::null_mut();
        let null_repo = |op: &str, failed: bool, error: &mut *mut c_char| {
            assert!(failed, "{op} succeeded with a NULL repo");
            assert_eq!(take_error(error), format!("{op}: repo is NULL"));
        };

        let blame = unsafe {
            rcgit_blame(
                ptr::null(),
                &raw const tip,
                path.as_ptr(),
                1 << 20,
                i64::MIN,
                &raw mut error,
            )
        };
        null_repo("rcgit_blame", blame.is_null(), &mut error);
        let read =
            unsafe { rcgit_blob_read(ptr::null(), &raw const blob, 1 << 20, &raw mut error) };
        null_repo("rcgit_blob_read", read.is_null(), &mut error);
        let status =
            unsafe { rcgit_blob_size(ptr::null(), &raw const blob, &raw mut size, &raw mut error) };
        null_repo("rcgit_blob_size", status == RCGIT_ERROR, &mut error);
        let commit = unsafe { rcgit_commit(ptr::null(), &raw const tip, &raw mut error) };
        null_repo("rcgit_commit", commit.is_null(), &mut error);
        let log = unsafe { rcgit_log_cursor_new(ptr::null(), &raw const tip, &raw mut error) };
        null_repo("rcgit_log_cursor_new", log.is_null(), &mut error);
        let refs = unsafe { rcgit_refs(ptr::null(), &raw mut error) };
        null_repo("rcgit_refs", refs.is_null(), &mut error);
        let status =
            unsafe { rcgit_resolve_tree(ptr::null(), spec.as_ptr(), &raw mut out, &raw mut error) };
        null_repo("rcgit_resolve_tree", status == RCGIT_ERROR, &mut error);
        let entries = unsafe { rcgit_tree_entries(ptr::null(), &raw const tree, &raw mut error) };
        null_repo("rcgit_tree_entries", entries.is_null(), &mut error);
        let file =
            unsafe { rcgit_worktree_read(ptr::null(), path.as_ptr(), 1 << 20, &raw mut error) };
        null_repo("rcgit_worktree_read", file.is_null(), &mut error);
        assert!(unsafe { rcgit_repo_work_dir(ptr::null()) }.is_null());
    }

    /// Each named free function, called with NULL.
    macro_rules! null_frees {
        ($($free:ident),* $(,)?) => {
            [$((stringify!($free), (|| unsafe { $free(ptr::null_mut()) }) as fn())),*]
        };
    }

    /// The `_free` functions the committed C header declares.
    fn header_frees() -> BTreeSet<String> {
        let header = include_str!("../../../mac/Sources/CRcGit/include/rc_git.h");
        let mut frees = BTreeSet::new();
        for (end, _) in header.match_indices("_free(") {
            let head = &header[..end];
            let start = head
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(0, |at| at + 1);
            frees.insert(format!("{}_free", &head[start..]));
        }
        frees
    }

    #[test]
    fn free_functions_accept_null() {
        let frees = null_frees![
            rcgit_blame_list_free,
            rcgit_blob_diff_free,
            rcgit_blob_free,
            rcgit_cancel_free,
            rcgit_commit_detail_free,
            rcgit_commit_free,
            rcgit_commit_list_free,
            rcgit_commit_page_free,
            rcgit_commit_result_free,
            rcgit_guard_decision_free,
            rcgit_head_free,
            rcgit_highlight_list_free,
            rcgit_history_walk_free,
            rcgit_identity_free,
            rcgit_import_link_list_free,
            rcgit_log_cursor_free,
            rcgit_ref_list_free,
            rcgit_remote_status_free,
            rcgit_repo_free,
            rcgit_stash_list_free,
            rcgit_status_list_free,
            rcgit_string_free,
            rcgit_tree_change_list_free,
            rcgit_tree_entry_free,
            rcgit_tree_list_free,
            rcgit_worktree_file_list_free,
            rcgit_worktree_list_free,
        ];
        let named: BTreeSet<String> = frees.iter().map(|(name, _)| (*name).to_owned()).collect();
        assert_eq!(
            named,
            header_frees(),
            "every free in the header is called with NULL"
        );
        for (_, free) in frees {
            free();
        }
        assert!(unsafe { rcgit_oid_hex(ptr::null()) }.is_null());
    }

    #[test]
    fn tree_diff_and_blob_diff() {
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
        assert_eq!(hunk_line_texts(hunk), [b"guide".as_slice()]);
        assert_eq!(lines[0].eol, RcGitLineEnd::Lf);
        unsafe { rcgit_blob_diff_free(diff) };

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
        assert_eq!(take_error(&mut error), "rcgit_diff_trees: to is NULL");

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
        assert_eq!(
            take_error(&mut error),
            "rcgit_diff_blobs: old is malformed (len 33)"
        );
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn diff_hunk_ships_line_bytes_in_one_buffer_unchanged() {
        let line = |kind, text: &[u8], eol| rc_git_core::DiffLine {
            kind,
            text: text.to_vec().into(),
            eol,
        };
        let hunk = RcGitDiffHunk::export(DiffHunk {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
            lines: vec![
                line(DiffLineKind::Context, b"same", LineEnd::Lf),
                line(DiffLineKind::Removed, b"", LineEnd::CrLf),
                line(DiffLineKind::Added, b"nul\0inside", LineEnd::None),
            ],
        });
        assert_eq!(
            hunk_line_texts(&hunk),
            [b"same".as_slice(), b"", b"nul\0inside"],
            "an interior NUL is kept, and an empty line is an empty slice"
        );
        assert_eq!(
            hunk.data_len, 14,
            "one buffer holds every line, no terminators"
        );
        let lines = unsafe { std::slice::from_raw_parts(hunk.lines, hunk.line_count) };
        let shape: Vec<_> = lines.iter().map(|l| (l.kind, l.eol, l.start)).collect();
        assert_eq!(
            shape,
            [
                (RcGitDiffLineKind::Context, RcGitLineEnd::Lf, 0),
                (RcGitDiffLineKind::Removed, RcGitLineEnd::CrLf, 4),
                (RcGitDiffLineKind::Added, RcGitLineEnd::None, 4),
            ]
        );
    }

    #[test]
    fn binary_and_too_large_diffs_carry_their_sizes() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let bin = resolve(repo, "main:bin.dat");
        let guide = resolve(repo, "main:docs/guide.md");
        let diff_blobs = |old: &RcGitOid, limit: u64, error: &mut *mut c_char| unsafe {
            rcgit_diff_blobs(repo, old, &raw const guide, limit, 3, error)
        };

        let diff = diff_blobs(&bin, 1 << 20, &mut error);
        let diff_ref = deref(diff, &mut error);
        assert_eq!(diff_ref.kind, RcGitBlobDiffKind::Binary);
        assert_eq!(
            (diff_ref.old_size, diff_ref.new_size),
            (BIN_DAT.len() as u64, 6)
        );
        unsafe { rcgit_blob_diff_free(diff) };

        let diff = diff_blobs(&guide, 2, &mut error);
        let diff_ref = deref(diff, &mut error);
        assert_eq!(diff_ref.kind, RcGitBlobDiffKind::TooLarge);
        assert_eq!((diff_ref.size, diff_ref.limit), (6, 2));
        unsafe { rcgit_blob_diff_free(diff) };
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn blame_reports_ranges_and_a_file_over_the_limit() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let new = resolve(repo, "main");

        let path = CString::new("README.md").unwrap();
        let blame_at = |since: i64, error: &mut *mut c_char| unsafe {
            rcgit_blame(repo, &raw const new, path.as_ptr(), 1 << 20, since, error)
        };
        let blame = blame_at(i64::MIN, &mut error);
        assert!(!blame.is_null(), "{}", take_error(&mut error));
        let blame_ref = unsafe { &*blame };
        assert_eq!(blame_ref.kind, RcGitBlameKind::Ranges);
        assert_eq!(blame_ref.len, 1);
        let range = unsafe { &*blame_ref.ranges };
        assert_eq!((range.start_line, range.line_count), (1, 3));
        assert_eq!(hex(&range.commit), fx.rev_parse("v0.9^{commit}"));
        assert!(!range.boundary, "i64::MIN is no floor");
        unsafe { rcgit_blame_list_free(blame) };

        let blame = blame_at(i64::MAX, &mut error);
        assert!(!blame.is_null(), "{}", take_error(&mut error));
        let range = unsafe { &*(*blame).ranges };
        assert_eq!(hex(&range.commit), fx.rev_parse("main"));
        assert!(range.boundary, "a floor past the tip marks every line");
        unsafe { rcgit_blame_list_free(blame) };

        let blame = unsafe {
            rcgit_blame(
                repo,
                &raw const new,
                path.as_ptr(),
                4,
                i64::MIN,
                &raw mut error,
            )
        };
        assert!(!blame.is_null(), "{}", take_error(&mut error));
        let blame_ref = unsafe { &*blame };
        assert_eq!(blame_ref.kind, RcGitBlameKind::TooLarge);
        assert_eq!((blame_ref.len, blame_ref.limit), (0, 4));
        assert!(blame_ref.ranges.is_null());
        assert_eq!(blame_ref.size, README.len() as u64);
        unsafe { rcgit_blame_list_free(blame) };

        let missing = CString::new("nope.txt").unwrap();
        let blame = unsafe {
            rcgit_blame(
                repo,
                &raw const new,
                missing.as_ptr(),
                1 << 20,
                i64::MIN,
                &raw mut error,
            )
        };
        assert!(blame.is_null());
        assert!(take_error(&mut error).contains("blame file"));
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn worktree_listing_and_file_reads() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tree = resolve(repo, "HEAD^{tree}");
        fx.write("untracked.txt", b"new\n");
        fx.remove("Zeta.txt");

        let dir = CString::new("").unwrap();
        let list =
            unsafe { rcgit_worktree_entries(repo, &raw const tree, dir.as_ptr(), &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        assert_eq!(list_ref.missing_len, 1);
        let missing = unsafe { std::slice::from_raw_parts(list_ref.missing, list_ref.missing_len) };
        assert_eq!(text(missing[0].name), "Zeta.txt");
        assert_eq!(hex(&missing[0].oid), fx.rev_parse("HEAD:Zeta.txt"));
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
        assert_eq!(
            take_error(&mut error),
            "rcgit_worktree_entries: tree is NULL"
        );
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn diff_limits_are_clamped_to_4_gib() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let old = resolve(repo, "HEAD:alpha.txt");
        let path = CString::new("alpha.txt").unwrap();

        let diff = unsafe {
            rcgit_diff_blobs(
                repo,
                ptr::null(),
                &raw const old,
                u64::MAX,
                3,
                &raw mut error,
            )
        };
        assert!(!diff.is_null(), "{}", take_error(&mut error));
        assert_eq!(unsafe { &*diff }.limit, u64::from(u32::MAX));
        unsafe { rcgit_blob_diff_free(diff) };

        let diff = unsafe {
            rcgit_diff_worktree_file(
                repo,
                &raw const old,
                path.as_ptr(),
                u64::MAX,
                3,
                &raw mut error,
            )
        };
        assert!(!diff.is_null(), "{}", take_error(&mut error));
        assert_eq!(unsafe { &*diff }.limit, u64::from(u32::MAX));
        unsafe { rcgit_blob_diff_free(diff) };
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
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        let entries = unsafe { std::slice::from_raw_parts(list_ref.entries, list_ref.len) };
        assert!(
            entries
                .iter()
                .all(|entry| entry.status == RcGitWorktreeStatus::Untracked && !entry.has_oid),
            "with no tree every file is untracked"
        );
        assert!(entries.iter().any(|entry| text(entry.path) == "alpha.txt"));
        unsafe { rcgit_status_list_free(list) };
        let list =
            unsafe { rcgit_worktree_status(ptr::null(), ptr::null(), 100_000, &raw mut error) };
        assert!(list.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_worktree_status: repo is NULL"
        );
        let diff = unsafe {
            rcgit_diff_worktree_file(repo, ptr::null(), ptr::null(), 1 << 20, 3, &raw mut error)
        };
        assert!(diff.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_diff_worktree_file: path is NULL"
        );
        let stashes = unsafe { rcgit_stash_list(ptr::null(), &raw mut error) };
        assert!(stashes.is_null());
        assert_eq!(take_error(&mut error), "rcgit_stash_list: repo is NULL");
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn worktree_files_cross_the_boundary() {
        let fx = Fixture::standard();
        fx.write(".gitignore", b".env\n");
        fx.write(".env", b"SECRET=1\n");
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let tree = resolve(repo, "HEAD^{tree}");

        let list = unsafe { rcgit_worktree_files(repo, &raw const tree, 100_000, &raw mut error) };
        assert!(!list.is_null(), "{}", take_error(&mut error));
        let list_ref = unsafe { &*list };
        assert!(!list_ref.truncated);
        assert!(list_ref.scanned >= list_ref.len);
        let files = unsafe { std::slice::from_raw_parts(list_ref.files, list_ref.len) };
        let rows: Vec<(String, RcGitEntryKind)> = files
            .iter()
            .map(|file| (text(file.path), file.kind))
            .collect();
        assert_eq!(rows[0], (".env".to_owned(), RcGitEntryKind::Blob));
        assert!(rows.contains(&("link".to_owned(), RcGitEntryKind::Symlink)));
        assert!(rows.contains(&("src/main.rs".to_owned(), RcGitEntryKind::Blob)));
        unsafe { rcgit_worktree_file_list_free(list) };

        let list = unsafe { rcgit_worktree_files(repo, ptr::null(), 100_000, &raw mut error) };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "rcgit_worktree_files: tree is NULL");
        unsafe { rcgit_repo_free(repo) };
    }

    #[test]
    fn remote_preflight_reports_upstream_or_its_absence() {
        let fx = Fixture::standard();
        let path = CString::new(fx.path().to_str().unwrap()).unwrap();
        let mut error = ptr::null_mut();
        let status = unsafe { rcgit_guard_remote_preflight(path.as_ptr(), &raw mut error) };
        assert!(!status.is_null(), "{}", take_error(&mut error));
        let status_ref = unsafe { &*status };
        assert!(!status_ref.has_upstream);
        assert_eq!(text(status_ref.branch), "main");
        unsafe { rcgit_remote_status_free(status) };

        fx.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
        fx.git(&["config", "branch.main.remote", "origin"]);
        fx.git(&["config", "branch.main.merge", "refs/heads/main"]);
        let status = unsafe { rcgit_guard_remote_preflight(path.as_ptr(), &raw mut error) };
        assert!(!status.is_null(), "{}", take_error(&mut error));
        let status_ref = unsafe { &*status };
        assert!(status_ref.has_upstream);
        assert_eq!(text(status_ref.remote), "origin");
        assert_eq!(text(status_ref.host), "example.com");
        assert_eq!(text(status_ref.url), "https://example.com/repo.git");
        assert!(!status_ref.uses_ssh);
        unsafe { rcgit_remote_status_free(status) };

        fx.git(&["update-ref", "refs/remotes/origin/main", "main~1"]);
        let status = unsafe { rcgit_guard_remote_preflight(path.as_ptr(), &raw mut error) };
        assert!(!status.is_null(), "{}", take_error(&mut error));
        let status_ref = unsafe { &*status };
        assert_eq!((status_ref.ahead, status_ref.behind), (1, 0));
        unsafe { rcgit_remote_status_free(status) };

        let upstream = fx.git_output(&["commit-tree", "main^{tree}", "-p", "main", "-m", "up"]);
        fx.git(&["update-ref", "refs/remotes/origin/main", upstream.trim()]);
        let status = unsafe { rcgit_guard_remote_preflight(path.as_ptr(), &raw mut error) };
        assert!(!status.is_null(), "{}", take_error(&mut error));
        let status_ref = unsafe { &*status };
        assert_eq!((status_ref.ahead, status_ref.behind), (0, 1));
        unsafe { rcgit_remote_status_free(status) };

        fx.git(&["remote", "set-url", "origin", "git@example.com:x/y.git"]);
        let status = unsafe { rcgit_guard_remote_preflight(path.as_ptr(), &raw mut error) };
        assert!(!status.is_null(), "{}", take_error(&mut error));
        let status_ref = unsafe { &*status };
        assert!(status_ref.uses_ssh);
        assert_eq!(text(status_ref.url), "example.com:x/y.git");
        unsafe { rcgit_remote_status_free(status) };

        let status = unsafe { rcgit_guard_remote_preflight(ptr::null(), &raw mut error) };
        assert!(status.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_remote_preflight: repo_path is NULL"
        );
    }

    #[test]
    fn remote_actions_reject_null_paths_and_map_status_codes() {
        let mut error = ptr::null_mut();
        let mut fetch_out = RcGitFetchOutcome { updated_refs: 0 };
        let code = unsafe {
            rcgit_guard_fetch(
                ptr::null(),
                ptr::null(),
                ptr::null(),
                &raw mut fetch_out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_fetch: repo_path is NULL"
        );

        let mut pull_out = RcGitPullOutcome {
            new_head: RcGitOid::empty(),
            files_changed: 0,
            already_up_to_date: false,
        };
        let code = unsafe { rcgit_guard_pull_ff(ptr::null(), &raw mut pull_out, &raw mut error) };
        assert_eq!(code, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_pull_ff: repo_path is NULL"
        );

        let mut push_out = RcGitPushOutcome {
            pushed_commits: 0,
            already_up_to_date: false,
        };
        let code = unsafe {
            rcgit_guard_push(
                ptr::null(),
                ptr::null(),
                ptr::null(),
                &raw mut push_out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_push: repo_path is NULL"
        );

        // A git:// remote is refused with RCGIT_ERROR before any connection is made.
        let fx = Fixture::standard();
        fx.git(&["remote", "add", "origin", "git://example.com/x.git"]);
        let path = CString::new(fx.path().to_str().unwrap()).unwrap();
        let code = unsafe {
            rcgit_guard_fetch(
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                &raw mut fetch_out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("git"));

        // A diverged pull maps to RCGIT_DIVERGED: fake a tracking ref behind a local
        // commit on both sides.
        let fx = Fixture::standard();
        fx.git(&["config", "branch.main.remote", "origin"]);
        fx.git(&["config", "branch.main.merge", "refs/heads/main"]);
        fx.git(&["remote", "add", "origin", "https://example.com/repo.git"]);
        fx.git(&["update-ref", "refs/remotes/origin/main", "feature"]);
        let path = CString::new(fx.path().to_str().unwrap()).unwrap();
        let code = unsafe { rcgit_guard_pull_ff(path.as_ptr(), &raw mut pull_out, &raw mut error) };
        assert_eq!(code, RCGIT_DIVERGED);
        assert!(take_error(&mut error).contains("cannot fast-forward"));
    }

    #[test]
    fn a_signalled_cancel_stops_a_remote_action_with_its_own_status() {
        let cancel = rcgit_cancel_new();
        assert!(!cancel.is_null());

        // A closed loopback port: a regression that dials fails at once, never leaving the machine.
        let fx = Fixture::standard();
        fx.git(&["remote", "add", "origin", "https://127.0.0.1:9/x.git"]);
        fx.git(&["config", "branch.main.remote", "origin"]);
        fx.git(&["config", "branch.main.merge", "refs/heads/main"]);
        let path = CString::new(fx.path().to_str().unwrap()).unwrap();
        let mut fetch_out = RcGitFetchOutcome { updated_refs: 0 };
        let mut error = ptr::null_mut();

        // Signalled from this thread before the call; a real caller signals from
        // another while this one is blocked inside the transfer.
        unsafe { rcgit_cancel_signal(cancel) };
        let code = unsafe {
            rcgit_guard_fetch(
                path.as_ptr(),
                ptr::null(),
                cancel,
                &raw mut fetch_out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_CANCELLED);
        assert!(take_error(&mut error).contains("cancelled"));

        let mut push_out = RcGitPushOutcome {
            pushed_commits: 0,
            already_up_to_date: false,
        };
        let code = unsafe {
            rcgit_guard_push(
                path.as_ptr(),
                ptr::null(),
                cancel,
                &raw mut push_out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_CANCELLED);
        assert!(take_error(&mut error).contains("cancelled"));

        unsafe { rcgit_cancel_free(cancel) };
        // NULL is accepted everywhere a flag is taken, and means "never cancelled".
        unsafe { rcgit_cancel_signal(ptr::null()) };
        unsafe { rcgit_cancel_free(ptr::null_mut()) };
    }

    #[test]
    fn ssh_unknown_hostkey_maps_to_its_status_and_an_accepted_retry_succeeds() {
        let origin = Fixture::standard();
        let (_bare, bare_path) = origin.bare_copy();
        let server = rc_git_testkit::SshGitServer::serve(&bare_path);
        let (clone, ssh_home) = Fixture::ssh_clone(&bare_path, &server);

        let path = CString::new(clone.path().to_str().unwrap()).unwrap();
        let ssh_dir = CString::new(ssh_home.path().to_str().unwrap()).unwrap();
        let auth = RcGitRemoteAuth {
            username: ptr::null(),
            token: ptr::null(),
            host: ptr::null(),
            accepted_hostkey: ptr::null(),
            ssh_dir: ssh_dir.as_ptr(),
        };
        let mut fetch_out = RcGitFetchOutcome { updated_refs: 0 };
        let mut hostkey = ptr::null_mut();
        let mut error = ptr::null_mut();
        let code = unsafe {
            rcgit_guard_fetch(
                path.as_ptr(),
                &raw const auth,
                ptr::null(),
                &raw mut fetch_out,
                &raw mut hostkey,
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_HOSTKEY_UNKNOWN);
        let offered = take_error(&mut hostkey);
        assert_eq!(
            offered,
            format!("ssh-ed25519 {}", server.host_fingerprint())
        );
        assert!(take_error(&mut error).contains("not a known host"));

        let accepted = CString::new(server.host_fingerprint()).unwrap();
        let auth = RcGitRemoteAuth {
            accepted_hostkey: accepted.as_ptr(),
            ..auth
        };
        let code = unsafe {
            rcgit_guard_fetch(
                path.as_ptr(),
                &raw const auth,
                ptr::null(),
                &raw mut fetch_out,
                &raw mut hostkey,
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_OK, "{}", take_error(&mut error));
        assert!(hostkey.is_null());
    }

    #[test]
    fn remote_auth_keeps_a_token_given_without_a_username() {
        let token = CString::new("passphrase").unwrap();
        let auth = RcGitRemoteAuth {
            username: ptr::null(),
            token: token.as_ptr(),
            host: ptr::null(),
            accepted_hostkey: ptr::null(),
            ssh_dir: ptr::null(),
        };
        let owned = unsafe { OwnedRemoteAuth::read(&raw const auth) };
        let creds = owned.creds.expect("a token without a username is kept");
        assert_eq!(creds.username, "");
        assert_eq!(creds.token, "passphrase");

        let auth = RcGitRemoteAuth {
            token: ptr::null(),
            ..auth
        };
        let owned = unsafe { OwnedRemoteAuth::read(&raw const auth) };
        assert!(owned.creds.is_none());
    }

    #[test]
    fn remote_auth_carries_the_credentials_host_to_the_guard() {
        let host = CString::new("example.com:8443").unwrap();
        let auth = RcGitRemoteAuth {
            username: ptr::null(),
            token: ptr::null(),
            host: host.as_ptr(),
            accepted_hostkey: ptr::null(),
            ssh_dir: ptr::null(),
        };
        let owned = unsafe { OwnedRemoteAuth::read(&raw const auth) };
        assert_eq!(owned.borrow().host, Some("example.com:8443"));

        let auth = RcGitRemoteAuth {
            host: ptr::null(),
            ..auth
        };
        let owned = unsafe { OwnedRemoteAuth::read(&raw const auth) };
        assert_eq!(owned.borrow().host, None);
    }

    /// Fetches `repo` through the C entry point; returns the status, the error message
    /// (empty when none was set), and `hostkey` when it was filled.
    fn ffi_fetch(repo: &Path, auth: Option<&RcGitRemoteAuth>) -> (i32, String, Option<String>) {
        let path = CString::new(repo.to_str().unwrap()).unwrap();
        let auth = auth.map_or(ptr::null(), ptr::from_ref);
        let mut out = RcGitFetchOutcome { updated_refs: 0 };
        let mut hostkey = ptr::null_mut();
        let mut error = ptr::null_mut();
        let code = unsafe {
            rcgit_guard_fetch(
                path.as_ptr(),
                auth,
                ptr::null(),
                &raw mut out,
                &raw mut hostkey,
                &raw mut error,
            )
        };
        let message = if error.is_null() {
            String::new()
        } else {
            take_error(&mut error)
        };
        let hostkey = (!hostkey.is_null()).then(|| take_error(&mut hostkey));
        (code, message, hostkey)
    }

    fn anonymous_fetch_from_an_auth_server_maps_to_auth_required() {
        let origin = Fixture::standard();
        let (_bare, bare_path) = origin.bare_copy();
        let server = rc_git_testkit::HttpGitServer::serve_with_auth(&bare_path, "user", "sekret");
        let clone = Fixture::basic_auth_clone(&server, "user", "sekret");

        let (code, message, hostkey) = ffi_fetch(clone.path(), None);
        assert_eq!(code, RCGIT_AUTH_REQUIRED, "{message}");
        assert!(message.contains("requires authentication"), "{message}");
        assert_eq!(hostkey, None);
    }

    fn encrypted_ssh_key_maps_to_passphrase_required_until_the_passphrase_is_given() {
        let origin = Fixture::standard();
        let (_bare, bare_path) = origin.bare_copy();
        let server = rc_git_testkit::SshGitServer::serve(&bare_path);
        let (clone, ssh_home) = Fixture::ssh_clone(&bare_path, &server);
        server.write_known_hosts(ssh_home.path());
        let passphrase = server.write_encrypted_client_key(ssh_home.path());

        let ssh_dir = CString::new(ssh_home.path().to_str().unwrap()).unwrap();
        let auth = RcGitRemoteAuth {
            username: ptr::null(),
            token: ptr::null(),
            host: ptr::null(),
            accepted_hostkey: ptr::null(),
            ssh_dir: ssh_dir.as_ptr(),
        };
        let (code, message, hostkey) = ffi_fetch(clone.path(), Some(&auth));
        assert_eq!(code, RCGIT_PASSPHRASE_REQUIRED, "{message}");
        let key = ssh_home.path().join("id_ed25519");
        assert_eq!(
            hostkey,
            Some(key.display().to_string()),
            "the locked key is named"
        );

        // The app sends an empty username with a passphrase; ssh ignores it.
        let username = CString::new("").unwrap();
        let token = CString::new(passphrase).unwrap();
        let auth = RcGitRemoteAuth {
            username: username.as_ptr(),
            token: token.as_ptr(),
            ..auth
        };
        let (code, message, _) = ffi_fetch(clone.path(), Some(&auth));
        assert_eq!(code, RCGIT_OK, "{message}");
    }

    fn push_behind_a_moved_remote_maps_to_error_not_diverged() {
        let origin = Fixture::standard();
        let (_bare, bare_path) = origin.bare_copy();
        let server = rc_git_testkit::HttpGitServer::serve(&bare_path);
        let clone = Fixture::empty_dir();
        clone.git(&["clone", "--quiet", &server.url(), "."]);
        origin.write("news.txt", b"news\n");
        origin.commit("News on origin", "2024-03-01T00:00:00+0000");
        origin.push_main_to(&bare_path);
        clone.write("local.txt", b"local\n");
        clone.commit("Local work", "2024-03-02T00:00:00+0000");

        let path = CString::new(clone.path().to_str().unwrap()).unwrap();
        let mut out = RcGitPushOutcome {
            pushed_commits: 0,
            already_up_to_date: false,
        };
        let mut error = ptr::null_mut();
        let code = unsafe {
            rcgit_guard_push(
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                &raw mut out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        let message = take_error(&mut error);
        assert_eq!(code, RCGIT_ERROR, "{message}");
        assert!(message.contains("fetch and pull first"), "{message}");
    }

    fn ffi_pull(repo: &CString) -> RcGitPullOutcome {
        let mut out = RcGitPullOutcome {
            new_head: RcGitOid::empty(),
            files_changed: 0,
            already_up_to_date: false,
        };
        let mut error = ptr::null_mut();
        let code = unsafe { rcgit_guard_pull_ff(repo.as_ptr(), &raw mut out, &raw mut error) };
        assert_eq!(code, RCGIT_OK, "{}", take_error(&mut error));
        out
    }

    fn ffi_push(repo: &CString) -> RcGitPushOutcome {
        let mut out = RcGitPushOutcome {
            pushed_commits: 0,
            already_up_to_date: false,
        };
        let mut error = ptr::null_mut();
        let code = unsafe {
            rcgit_guard_push(
                repo.as_ptr(),
                ptr::null(),
                ptr::null(),
                &raw mut out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_OK, "{}", take_error(&mut error));
        out
    }

    #[test]
    fn sync_outcomes_cross_the_boundary() {
        let origin = Fixture::standard();
        let (_bare, bare_path) = origin.bare_copy();
        let server = rc_git_testkit::HttpGitServer::serve(&bare_path);
        let clone = Fixture::empty_dir();
        clone.git(&["clone", "--quiet", &server.url(), "."]);
        origin.write("news.txt", b"news\n");
        origin.commit("News on origin", "2024-03-01T00:00:00+0000");
        origin.push_main_to(&bare_path);
        let path = CString::new(clone.path().to_str().unwrap()).unwrap();

        let mut fetch_out = RcGitFetchOutcome { updated_refs: 0 };
        let mut error = ptr::null_mut();
        let code = unsafe {
            rcgit_guard_fetch(
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                &raw mut fetch_out,
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(code, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(fetch_out.updated_refs, 1);

        let pull_out = ffi_pull(&path);
        assert!(!pull_out.already_up_to_date);
        assert_eq!(pull_out.files_changed, 1);
        assert_eq!(hex(&pull_out.new_head), origin.rev_parse("HEAD"));
        assert!(ffi_pull(&path).already_up_to_date);

        clone.write("local.txt", b"local\n");
        clone.commit("Local work", "2024-03-02T00:00:00+0000");
        let push_out = ffi_push(&path);
        assert!(!push_out.already_up_to_date);
        assert_eq!(push_out.pushed_commits, 1);
        assert!(ffi_push(&path).already_up_to_date);
    }

    #[test]
    fn remote_failures_map_to_auth_and_diverged_statuses() {
        anonymous_fetch_from_an_auth_server_maps_to_auth_required();
        encrypted_ssh_key_maps_to_passphrase_required_until_the_passphrase_is_given();
        push_behind_a_moved_remote_maps_to_error_not_diverged();
    }

    #[test]
    fn guard_plans_carry_the_changes_tab_policy() {
        let plan = rcgit_guard_plan_discard(3);
        let plan_ref = unsafe { &*plan };
        assert_eq!(plan_ref.kind, RcGitDecisionKind::Confirm);
        assert!(
            plan_ref.typed_phrase.is_null(),
            "discard is answered by a plain confirm"
        );
        unsafe { rcgit_guard_decision_free(plan) };
        let message = CString::new("On main: wip").unwrap();
        let plan = unsafe { rcgit_guard_plan_stash_drop(1, message.as_ptr()) };
        let plan_ref = unsafe { &*plan };
        assert_eq!(text(plan_ref.typed_phrase), "drop");
        assert!(plan_ref.backup_ref);
        assert!(
            text(plan_ref.summary).contains("stash@{1} \"On main: wip\""),
            "the challenge names the entry"
        );
        unsafe { rcgit_guard_decision_free(plan) };
        assert!(unsafe { rcgit_guard_plan_stash_drop(1, ptr::null()) }.is_null());
        let plan = rcgit_guard_plan_stash_push(2);
        assert_eq!(unsafe { &*plan }.kind, RcGitDecisionKind::Confirm);
        unsafe { rcgit_guard_decision_free(plan) };
        let plan = rcgit_guard_plan_stash_apply();
        assert_eq!(unsafe { &*plan }.kind, RcGitDecisionKind::Allow);
        unsafe { rcgit_guard_decision_free(plan) };
        let plan = unsafe { rcgit_guard_plan_stash_pop(0, message.as_ptr()) };
        assert!(unsafe { &*plan }.backup_ref);
        assert!(text(unsafe { &*plan }.summary).contains("stash@{0} \"On main: wip\""));
        unsafe { rcgit_guard_decision_free(plan) };
        assert!(unsafe { rcgit_guard_plan_stash_pop(0, ptr::null()) }.is_null());
        let plan = rcgit_guard_plan_commit(2);
        assert_eq!(unsafe { &*plan }.kind, RcGitDecisionKind::Allow);
        unsafe { rcgit_guard_decision_free(plan) };
    }

    #[test]
    fn stash_actions_refuse_an_entry_other_than_the_one_shown() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"stashed\n");
        fx.git(&["stash", "push", "-q", "-m", "shown"]);
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let other = commit_id(&fx.rev_parse("HEAD"));
        let drop = CString::new("drop").unwrap();
        let mut backup = ptr::null_mut();
        let mut error = ptr::null_mut();

        let status = unsafe {
            rcgit_guard_stash_apply(
                repo_path.as_ptr(),
                0,
                &raw const other,
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("changed since the list was built"));
        let status = unsafe {
            rcgit_guard_stash_drop(
                repo_path.as_ptr(),
                0,
                &raw const other,
                drop.as_ptr(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("changed since the list was built"));
        let status = unsafe {
            rcgit_guard_stash_pop(
                repo_path.as_ptr(),
                0,
                ptr::null(),
                ptr::null(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_stash_pop: expected is NULL"
        );
        assert!(backup.is_null());
        assert_eq!(
            fx.git_output(&["stash", "list", "--format=%gs"]),
            "On main: shown"
        );
    }

    #[test]
    fn guarded_commit_stash_and_discard_cross_the_boundary() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();

        // Commit one whole file.
        fx.write("alpha.txt", b"via ffi\n");
        let head = commit_id(&fx.rev_parse("HEAD"));
        let message = CString::new("FFI commit").unwrap();
        let name = CString::new("FFI User").unwrap();
        let email = CString::new("ffi@example.com").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let file = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        let mut result = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: &RcGitFallbackIdentity {
                        name: name.as_ptr(),
                        email: email.as_ptr(),
                    },
                    expected_head: &raw const head,
                    files: &raw const file,
                    file_count: 1,
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        let result_ref = unsafe { &*result };
        assert_eq!(text(result_ref.branch), "main");
        assert_eq!(hex(&result_ref.oid), fx.rev_parse("HEAD"));
        unsafe { rcgit_commit_result_free(result) };
        assert_eq!(fx.git_output(&["log", "-1", "--format=%s"]), "FFI commit");

        // Stash push and pop round-trip.
        fx.write("alpha.txt", b"stash via ffi\n");
        let paths = [path.as_ptr()];
        let status = unsafe {
            rcgit_guard_stash_push(
                repo_path.as_ptr(),
                &RcGitStashRequest {
                    paths: paths.as_ptr(),
                    path_count: 1,
                    message: ptr::null(),
                    fallback: &RcGitFallbackIdentity {
                        name: name.as_ptr(),
                        email: email.as_ptr(),
                    },
                },
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"via ffi\n"
        );
        let mut backup = ptr::null_mut();
        let shown = commit_id(&fx.rev_parse("stash@{0}"));
        let status = unsafe {
            rcgit_guard_stash_pop(
                repo_path.as_ptr(),
                0,
                &raw const shown,
                ptr::null(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(take_string(backup), "refs/rc-git/backup/stash/0");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"stash via ffi\n"
        );

        // Discard needs no phrase: a NULL input restores.
        let status = unsafe {
            rcgit_guard_discard(
                repo_path.as_ptr(),
                paths.as_ptr(),
                1,
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"via ffi\n"
        );
    }

    #[test]
    fn the_fallback_identity_never_overrides_the_repository_config() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();

        let none = unsafe { rcgit_guard_configured_identity(repo_path.as_ptr(), &raw mut error) };
        assert!(none.is_null());
        assert!(error.is_null(), "an unset identity is not an error");

        fx.git(&["config", "user.name", "Repo Owner"]);
        fx.git(&["config", "user.email", "repo@example.com"]);
        let identity =
            unsafe { rcgit_guard_configured_identity(repo_path.as_ptr(), &raw mut error) };
        assert!(!identity.is_null(), "{}", take_error(&mut error));
        let identity_ref = unsafe { &*identity };
        assert_eq!(text(identity_ref.name), "Repo Owner");
        assert_eq!(text(identity_ref.email), "repo@example.com");
        unsafe { rcgit_identity_free(identity) };

        fx.write("alpha.txt", b"fallback loses\n");
        let head = commit_id(&fx.rev_parse("HEAD"));
        let message = CString::new("Repo identity wins").unwrap();
        let name = CString::new("Saved Fallback").unwrap();
        let email = CString::new("saved@example.com").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let file = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        let mut result = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: &RcGitFallbackIdentity {
                        name: name.as_ptr(),
                        email: email.as_ptr(),
                    },
                    expected_head: &raw const head,
                    files: &raw const file,
                    file_count: 1,
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        unsafe { rcgit_commit_result_free(result) };
        assert_eq!(
            fx.git_output(&["log", "-1", "--format=%ae|%ce"]),
            "repo@example.com|repo@example.com"
        );

        let missing = unsafe { rcgit_guard_configured_identity(ptr::null(), &raw mut error) };
        assert!(missing.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_configured_identity: repo_path is NULL"
        );
    }

    #[test]
    fn a_missing_identity_crosses_as_its_own_status() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let head = fx.rev_parse("HEAD");
        let head_c = commit_id(&head);
        let message = CString::new("nobody").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let file = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        fx.write("alpha.txt", b"no identity\n");
        let mut error = ptr::null_mut();
        let mut result = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: ptr::null(),
                    expected_head: &raw const head_c,
                    files: &raw const file,
                    file_count: 1,
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_IDENTITY_REQUIRED);
        assert!(result.is_null());
        assert!(take_error(&mut error).contains("no author identity"));
        assert_eq!(
            fx.rev_parse("HEAD"),
            head,
            "a refused commit writes nothing"
        );

        let paths = [path.as_ptr()];
        let status = unsafe {
            rcgit_guard_stash_push(
                repo_path.as_ptr(),
                &RcGitStashRequest {
                    paths: paths.as_ptr(),
                    path_count: 1,
                    message: ptr::null(),
                    fallback: ptr::null(),
                },
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_IDENTITY_REQUIRED);
        assert!(take_error(&mut error).contains("no author identity"));
        assert_eq!(fx.git_output(&["stash", "list"]), "");

        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: ptr::null(),
                    expected_head: &raw const head_c,
                    files: &raw const file,
                    file_count: 1,
                },
                ptr::null(),
                ptr::null_mut(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR, "a NULL out is an argument error");
        assert_eq!(take_error(&mut error), "rcgit_guard_commit: out is NULL");
    }

    #[test]
    fn a_malformed_expected_head_is_an_argument_error_not_a_moved_head() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let head = fx.rev_parse("HEAD");
        let message = CString::new("msg").unwrap();
        let name = CString::new("FFI User").unwrap();
        let email = CString::new("ffi@example.com").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let file = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        fx.write("alpha.txt", b"changed\n");
        let commit_against = |expected: &RcGitOid| {
            let mut error = ptr::null_mut();
            let mut result = ptr::null_mut();
            let status = unsafe {
                rcgit_guard_commit(
                    repo_path.as_ptr(),
                    &RcGitCommitRequest {
                        message: message.as_ptr(),
                        fallback: &RcGitFallbackIdentity {
                            name: name.as_ptr(),
                            email: email.as_ptr(),
                        },
                        expected_head: expected,
                        files: &raw const file,
                        file_count: 1,
                    },
                    ptr::null(),
                    &raw mut result,
                    &raw mut error,
                )
            };
            assert!(result.is_null());
            (status, take_error(&mut error))
        };

        let truncated = RcGitOid {
            bytes: [0; 32],
            len: 3,
        };
        let (status, message) = commit_against(&truncated);
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            message,
            "rcgit_guard_commit: request.expected_head is malformed (len 3)"
        );

        let (status, message) = commit_against(&commit_id(&fx.rev_parse("HEAD~1")));
        assert_eq!(status, RCGIT_ERROR);
        assert!(!message.contains("malformed"), "{message}");
        assert_eq!(
            fx.rev_parse("HEAD"),
            head,
            "a refused commit writes nothing"
        );
    }

    #[test]
    fn tag_create_stash_apply_and_drop_cross_the_boundary() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();

        let tag = CString::new("v2.0").unwrap();
        let create = |rev: &str, error: &mut *mut c_char| {
            let rev = CString::new(rev).unwrap();
            unsafe { rcgit_guard_tag_create(repo_path.as_ptr(), tag.as_ptr(), rev.as_ptr(), error) }
        };
        assert_eq!(
            create("main~1", &mut error),
            RCGIT_OK,
            "{}",
            take_error(&mut error)
        );
        assert!(error.is_null(), "success must leave error untouched");
        assert_eq!(fx.rev_parse("refs/tags/v2.0"), fx.rev_parse("main~1"));
        assert_eq!(
            create("main", &mut error),
            RCGIT_ERROR,
            "an existing tag is never moved"
        );
        assert!(take_error(&mut error).contains("v2.0"));
        assert_eq!(fx.rev_parse("refs/tags/v2.0"), fx.rev_parse("main~1"));

        fx.write("alpha.txt", b"stashed\n");
        fx.git(&["stash", "push", "-q", "-m", "kept"]);
        let shown = commit_id(&fx.rev_parse("stash@{0}"));
        let status = unsafe {
            rcgit_guard_stash_apply(
                repo_path.as_ptr(),
                0,
                &raw const shown,
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"stashed\n"
        );
        assert_eq!(
            fx.git_output(&["stash", "list", "--format=%gs"]),
            "On main: kept",
            "apply keeps the entry"
        );

        let drop = CString::new("drop").unwrap();
        let mut backup = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_stash_drop(
                repo_path.as_ptr(),
                0,
                &raw const shown,
                drop.as_ptr(),
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert_eq!(take_string(backup), "refs/rc-git/backup/stash/0");
        assert_eq!(fx.git_output(&["stash", "list"]), "");
    }

    #[test]
    fn guarded_actions_report_a_null_repo_path() {
        let name = CString::new("feature").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let paths = [path.as_ptr()];
        let oid = commit_id("0123456789abcdef0123456789abcdef01234567");
        let stash = RcGitStashRequest {
            paths: paths.as_ptr(),
            path_count: 1,
            message: ptr::null(),
            fallback: ptr::null(),
        };
        let mut backup = ptr::null_mut();
        let mut error = ptr::null_mut();
        let null_path = |op: &str, status: i32, error: &mut *mut c_char| {
            assert_eq!(status, RCGIT_ERROR, "{op}");
            assert_eq!(take_error(error), format!("{op}: repo_path is NULL"));
        };
        let p = ptr::null();

        let plan = unsafe { rcgit_guard_plan_branch_delete(p, name.as_ptr(), &raw mut error) };
        assert!(plan.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_plan_branch_delete: repo_path is NULL"
        );
        let status = unsafe {
            rcgit_guard_branch_delete(p, name.as_ptr(), p, &raw mut backup, &raw mut error)
        };
        null_path("rcgit_guard_branch_delete", status, &mut error);
        let status =
            unsafe { rcgit_guard_tag_delete(p, name.as_ptr(), p, &raw mut backup, &raw mut error) };
        null_path("rcgit_guard_tag_delete", status, &mut error);
        let status =
            unsafe { rcgit_guard_tag_create(p, name.as_ptr(), name.as_ptr(), &raw mut error) };
        null_path("rcgit_guard_tag_create", status, &mut error);
        let status = unsafe { rcgit_guard_discard(p, paths.as_ptr(), 1, p, &raw mut error) };
        null_path("rcgit_guard_discard", status, &mut error);
        let status = unsafe { rcgit_guard_stash_push(p, &raw const stash, p, &raw mut error) };
        null_path("rcgit_guard_stash_push", status, &mut error);
        let status = unsafe { rcgit_guard_stash_apply(p, 0, &raw const oid, p, &raw mut error) };
        null_path("rcgit_guard_stash_apply", status, &mut error);
        let status = unsafe {
            rcgit_guard_stash_pop(p, 0, &raw const oid, p, &raw mut backup, &raw mut error)
        };
        null_path("rcgit_guard_stash_pop", status, &mut error);
        let status = unsafe {
            rcgit_guard_stash_drop(p, 0, &raw const oid, p, &raw mut backup, &raw mut error)
        };
        null_path("rcgit_guard_stash_drop", status, &mut error);
        assert!(backup.is_null(), "a refused call writes no backup name");

        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let status = unsafe {
            rcgit_guard_stash_drop(
                repo_path.as_ptr(),
                0,
                ptr::null(),
                p,
                &raw mut backup,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_stash_drop: expected is NULL"
        );
        let status = unsafe {
            rcgit_guard_stash_apply(repo_path.as_ptr(), 0, ptr::null(), p, &raw mut error)
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_stash_apply: expected is NULL"
        );
        let status =
            unsafe { rcgit_guard_tag_create(repo_path.as_ptr(), p, name.as_ptr(), &raw mut error) };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_tag_create: name is NULL"
        );
    }

    /// Discards `paths` through the C entry point; returns the status and the error message
    /// (empty when none was set).
    fn ffi_discard(repo_path: &CString, paths: *const *const c_char, len: usize) -> (i32, String) {
        let mut error = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_discard(repo_path.as_ptr(), paths, len, ptr::null(), &raw mut error)
        };
        let message = if error.is_null() {
            String::new()
        } else {
            take_error(&mut error)
        };
        (status, message)
    }

    /// Stashes `paths` through the C entry point; returns as `ffi_discard`.
    fn ffi_stash_push(
        repo_path: &CString,
        paths: *const *const c_char,
        len: usize,
    ) -> (i32, String) {
        let name = CString::new("FFI User").unwrap();
        let email = CString::new("ffi@example.com").unwrap();
        let fallback = RcGitFallbackIdentity {
            name: name.as_ptr(),
            email: email.as_ptr(),
        };
        let request = RcGitStashRequest {
            paths,
            path_count: len,
            message: ptr::null(),
            fallback: &raw const fallback,
        };
        let mut error = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_stash_push(
                repo_path.as_ptr(),
                &raw const request,
                ptr::null(),
                &raw mut error,
            )
        };
        let message = if error.is_null() {
            String::new()
        } else {
            take_error(&mut error)
        };
        (status, message)
    }

    /// Commits `message` and `files` through the C entry point, with a fallback identity, against
    /// the current `HEAD`; asserts the commit is refused and returns its error message.
    fn ffi_commit_refused(
        fx: &Fixture,
        message: *const c_char,
        files: *const RcGitCommitFile,
        len: usize,
    ) -> String {
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let head = commit_id(&fx.rev_parse("HEAD"));
        let name = CString::new("FFI User").unwrap();
        let email = CString::new("ffi@example.com").unwrap();
        let mut result = ptr::null_mut();
        let mut error = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message,
                    fallback: &RcGitFallbackIdentity {
                        name: name.as_ptr(),
                        email: email.as_ptr(),
                    },
                    expected_head: &raw const head,
                    files,
                    file_count: len,
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(result.is_null());
        take_error(&mut error)
    }

    #[test]
    fn selection_arrays_reject_null_entries() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        fx.write("alpha.txt", b"kept\n");
        let null_entry = [ptr::null::<c_char>()];

        let refused = [
            (
                ffi_discard(&repo_path, null_entry.as_ptr(), 1),
                "rcgit_guard_discard: paths[0] is NULL",
            ),
            (
                ffi_discard(&repo_path, ptr::null(), 2),
                "rcgit_guard_discard: paths is NULL",
            ),
            (
                ffi_stash_push(&repo_path, null_entry.as_ptr(), 1),
                "rcgit_guard_stash_push: request.paths[0] is NULL",
            ),
            (
                ffi_stash_push(&repo_path, ptr::null(), 2),
                "rcgit_guard_stash_push: request.paths is NULL",
            ),
        ];
        for ((status, message), expected) in refused {
            assert_eq!(status, RCGIT_ERROR, "{expected}");
            assert_eq!(message, expected);
        }
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"kept\n"
        );
        assert_eq!(fx.git_output(&["stash", "list"]), "");

        for (status, message) in [
            ffi_discard(&repo_path, ptr::null(), 0),
            ffi_stash_push(&repo_path, ptr::null(), 0),
        ] {
            assert!(
                status == RCGIT_OK || !message.contains("is NULL"),
                "an empty list reaches the guard: {message}"
            );
        }

        let message = CString::new("msg").unwrap();
        let null_path = RcGitCommitFile {
            path: ptr::null(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        let path = CString::new("alpha.txt").unwrap();
        let whole = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        assert_eq!(
            ffi_commit_refused(&fx, message.as_ptr(), &raw const null_path, 1),
            "rcgit_guard_commit: request.files[0].path is NULL"
        );
        assert_eq!(
            ffi_commit_refused(&fx, message.as_ptr(), ptr::null(), 1),
            "rcgit_guard_commit: request.files is NULL"
        );
        assert_eq!(
            ffi_commit_refused(&fx, ptr::null(), &raw const whole, 1),
            "rcgit_guard_commit: request.message is NULL"
        );
    }

    #[test]
    fn guarded_mutations_report_null_arguments() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let message = CString::new("msg").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let mut error = ptr::null_mut();
        let mut result = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_commit(
                ptr::null(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: ptr::null(),
                    expected_head: ptr::null(),
                    files: ptr::null(),
                    file_count: 0,
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(result.is_null());
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_commit: repo_path is NULL"
        );

        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                ptr::null(),
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR, "a NULL request is an argument error");
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_commit: request is NULL"
        );

        let status = unsafe {
            rcgit_guard_discard_hunks(
                repo_path.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                0,
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_discard_hunks: stamps is empty"
        );

        let status = unsafe {
            rcgit_guard_discard_hunks(
                repo_path.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                2,
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_discard_hunks: stamps is NULL"
        );

        let paths = [path.as_ptr(), ptr::null()];
        let status = unsafe {
            rcgit_guard_discard(
                repo_path.as_ptr(),
                paths.as_ptr(),
                paths.len(),
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_discard: paths[1] is NULL"
        );
    }

    #[test]
    fn a_half_filled_fallback_identity_is_refused() {
        let fx = Fixture::standard();
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let message = CString::new("msg").unwrap();
        let path = CString::new("alpha.txt").unwrap();
        let mut error = ptr::null_mut();
        let mut result = ptr::null_mut();

        fx.write("alpha.txt", b"half an identity\n");
        let head = fx.rev_parse("HEAD");
        let head_c = commit_id(&head);
        let name = CString::new("Only A Name").unwrap();
        let half = RcGitFallbackIdentity {
            name: name.as_ptr(),
            email: ptr::null(),
        };
        let file = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 0,
        };
        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: &raw const half,
                    expected_head: &raw const head_c,
                    files: &raw const file,
                    file_count: 1,
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        assert_eq!(
            status, RCGIT_ERROR,
            "half a fallback is refused, not dropped"
        );
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_commit: request.fallback.email is NULL"
        );
        assert_eq!(
            fx.rev_parse("HEAD"),
            head,
            "a refused commit writes nothing"
        );

        let paths = [path.as_ptr()];
        let status = unsafe {
            rcgit_guard_stash_push(
                repo_path.as_ptr(),
                &RcGitStashRequest {
                    paths: paths.as_ptr(),
                    path_count: 1,
                    message: ptr::null(),
                    fallback: &raw const half,
                },
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_stash_push: request.fallback.email is NULL"
        );
        let status = unsafe {
            rcgit_guard_stash_push(repo_path.as_ptr(), ptr::null(), ptr::null(), &raw mut error)
        };
        assert_eq!(status, RCGIT_ERROR, "a NULL request is an argument error");
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_stash_push: request is NULL"
        );
        assert_eq!(fx.git_output(&["stash", "list"]), "");
    }

    /// The worktree hunks of `path` against `HEAD`, as stamps, read through the FFI.
    fn worktree_stamps(fx: &Fixture, path: &str) -> Vec<RcGitHunkStamp> {
        let repo = open(fx);
        let old = resolve(repo, &format!("HEAD:{path}"));
        let c_path = CString::new(path).unwrap();
        let mut error = ptr::null_mut();
        let diff = unsafe {
            rcgit_diff_worktree_file(
                repo,
                &raw const old,
                c_path.as_ptr(),
                1 << 20,
                3,
                &raw mut error,
            )
        };
        assert!(!diff.is_null(), "{}", take_error(&mut error));
        let diff_ref = unsafe { &*diff };
        assert_eq!(diff_ref.kind, RcGitBlobDiffKind::Text);
        let hunks = unsafe { std::slice::from_raw_parts(diff_ref.hunks, diff_ref.hunk_count) };
        let stamps = hunks
            .iter()
            .map(|hunk| RcGitHunkStamp {
                old_start: hunk.old_start,
                old_lines: hunk.old_lines,
                new_start: hunk.new_start,
                new_lines: hunk.new_lines,
            })
            .collect();
        unsafe { rcgit_blob_diff_free(diff) };
        unsafe { rcgit_repo_free(repo) };
        stamps
    }

    /// Commits `files` through `rcgit_guard_commit` against the current `HEAD`.
    fn guarded_commit(fx: &Fixture, files: &[RcGitCommitFile]) -> Result<String, String> {
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let head = commit_id(&fx.rev_parse("HEAD"));
        let message = CString::new("FFI hunks").unwrap();
        let name = CString::new("FFI User").unwrap();
        let email = CString::new("ffi@example.com").unwrap();
        let mut error = ptr::null_mut();
        let mut result = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_commit(
                repo_path.as_ptr(),
                &RcGitCommitRequest {
                    message: message.as_ptr(),
                    fallback: &RcGitFallbackIdentity {
                        name: name.as_ptr(),
                        email: email.as_ptr(),
                    },
                    expected_head: &raw const head,
                    files: files.as_ptr(),
                    file_count: files.len(),
                },
                ptr::null(),
                &raw mut result,
                &raw mut error,
            )
        };
        if status != RCGIT_OK {
            return Err(take_error(&mut error));
        }
        let oid = hex(&unsafe { &*result }.oid);
        unsafe { rcgit_commit_result_free(result) };
        Ok(oid)
    }

    #[test]
    fn hunk_selections_cross_the_boundary() {
        let fx = Fixture::standard();
        let original: String = (1..=20)
            .map(|n| ["l", &n.to_string(), "\n"].concat())
            .collect();
        fx.write("hunky.txt", original.as_bytes());
        fx.commit("Add hunky", "2024-01-05T00:00:00+0000");
        let edited =
            original
                .replacen("l1\n", "l1\nTOP1\nTOP2\n", 1)
                .replacen("l18\n", "L18a\nL18b\n", 1);
        fx.write("hunky.txt", edited.as_bytes());

        let stamps = worktree_stamps(&fx, "hunky.txt");
        assert_eq!(stamps.len(), 2);
        let second = &stamps[1];
        assert_eq!(
            [
                second.old_start,
                second.old_lines,
                second.new_start,
                second.new_lines
            ],
            [15, 6, 17, 7],
            "every field differs, so a swap across the boundary is a mismatch"
        );
        let path = CString::new("hunky.txt").unwrap();
        let file = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: second,
            hunk_count: 1,
        };
        let oid = guarded_commit(&fx, &[file]).unwrap();
        assert_eq!(oid, fx.rev_parse("HEAD"));
        let committed = fx.git_output(&["show", "HEAD:hunky.txt"]);
        assert!(committed.contains("L18a\nL18b"), "{committed}");
        assert!(!committed.contains("TOP1"), "{committed}");

        let remaining = worktree_stamps(&fx, "hunky.txt");
        assert_eq!(remaining.len(), 1);
        let repo_path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let mut error = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_discard_hunks(
                repo_path.as_ptr(),
                path.as_ptr(),
                remaining.as_ptr(),
                remaining.len(),
                ptr::null(),
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        let on_disk = std::fs::read_to_string(fx.path().join("hunky.txt")).unwrap();
        assert_eq!(on_disk.trim_end(), committed);
    }

    #[test]
    fn an_empty_or_uncounted_hunk_list_never_commits_the_whole_file() {
        let fx = Fixture::standard();
        fx.write("alpha.txt", b"changed\n");
        let head = fx.rev_parse("HEAD");
        let path = CString::new("alpha.txt").unwrap();
        let stamp = RcGitHunkStamp {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
        };

        let empty = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: &raw const stamp,
            hunk_count: 0,
        };
        let err = guarded_commit(&fx, &[empty]).unwrap_err();
        assert!(err.contains("no hunks selected for 'alpha.txt'"), "{err}");
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");

        let uncounted = RcGitCommitFile {
            path: path.as_ptr(),
            hunks: ptr::null(),
            hunk_count: 1,
        };
        let err = guarded_commit(&fx, &[uncounted]).unwrap_err();
        assert_eq!(err, "rcgit_guard_commit: request.files[0].hunks is NULL");
        assert_eq!(fx.rev_parse("HEAD"), head, "nothing moved");
        assert_eq!(
            std::fs::read(fx.path().join("alpha.txt")).unwrap(),
            b"changed\n"
        );
    }

    #[test]
    fn highlighting_crosses_the_boundary() {
        let mut error = ptr::null_mut();
        let name = CString::new("main.rs").unwrap();
        let data = b"fn main() {}\n";
        let list = unsafe {
            rcgit_highlight(
                name.as_ptr(),
                data.as_ptr(),
                data.len(),
                ptr::null(),
                &raw mut error,
            )
        };
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

        let fence = CString::new("rust").unwrap();
        let list = unsafe {
            rcgit_highlight(
                fence.as_ptr(),
                data.as_ptr(),
                data.len(),
                ptr::null(),
                &raw mut error,
            )
        };
        assert!(!list.is_null(), "a fence info string names a language too");
        assert!(error.is_null());
        unsafe { rcgit_highlight_list_free(list) };

        let unknown = CString::new("photo.png").unwrap();
        let list = unsafe {
            rcgit_highlight(
                unknown.as_ptr(),
                data.as_ptr(),
                data.len(),
                ptr::null(),
                &raw mut error,
            )
        };
        assert!(list.is_null());
        assert!(error.is_null(), "an unknown language is not an error");

        let list =
            unsafe { rcgit_highlight(ptr::null(), data.as_ptr(), 0, ptr::null(), &raw mut error) };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "rcgit_highlight: name is NULL");

        let list =
            unsafe { rcgit_highlight(name.as_ptr(), ptr::null(), 0, ptr::null(), &raw mut error) };
        assert!(!list.is_null(), "no bytes means no spans");
        assert_eq!(unsafe { &*list }.len, 0);
        assert!(error.is_null());
        unsafe { rcgit_highlight_list_free(list) };

        let list =
            unsafe { rcgit_highlight(name.as_ptr(), ptr::null(), 5, ptr::null(), &raw mut error) };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "rcgit_highlight: data is NULL");
    }

    #[test]
    fn a_language_is_named_by_the_file_extension() {
        let mut error = ptr::null_mut();
        for (file, expected) in [("README.MD", "markdown"), ("notes.markdown", "markdown")] {
            let name = CString::new(file).unwrap();
            let language = unsafe { rcgit_language_name(name.as_ptr(), &raw mut error) };
            assert_eq!(take_string(language), expected, "{file}");
            assert!(error.is_null(), "success must leave error untouched");
        }

        for file in ["photo.png", "markdown", "rust"] {
            let name = CString::new(file).unwrap();
            let language = unsafe { rcgit_language_name(name.as_ptr(), &raw mut error) };
            assert!(language.is_null(), "{file} names no language by extension");
            assert!(error.is_null(), "an unknown language is not an error");
        }

        let language = unsafe { rcgit_language_name(ptr::null(), &raw mut error) };
        assert!(language.is_null());
        assert_eq!(take_error(&mut error), "rcgit_language_name: name is NULL");
    }

    #[test]
    fn a_signalled_cancel_stops_highlighting() {
        let mut error = ptr::null_mut();
        let name = CString::new("main.rs").unwrap();
        let data = b"fn main() {}\n";
        let cancel = rcgit_cancel_new();
        let list = unsafe {
            rcgit_highlight(
                name.as_ptr(),
                data.as_ptr(),
                data.len(),
                cancel,
                &raw mut error,
            )
        };
        assert!(!list.is_null(), "an unsignalled flag changes nothing");
        assert!(error.is_null());
        unsafe { rcgit_highlight_list_free(list) };

        unsafe { rcgit_cancel_signal(cancel) };
        let list = unsafe {
            rcgit_highlight(
                name.as_ptr(),
                data.as_ptr(),
                data.len(),
                cancel,
                &raw mut error,
            )
        };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "highlighting rust: Cancelled");
        unsafe { rcgit_cancel_free(cancel) };
    }

    #[test]
    fn import_links_cross_the_boundary() {
        let mut error = ptr::null_mut();
        let name = CString::new("App.tsx").unwrap();
        let data = b"import Panel from './Panel';\nconst view = <Panel />;\n";
        let list =
            unsafe { rcgit_import_links(name.as_ptr(), data.as_ptr(), data.len(), &raw mut error) };
        assert!(!list.is_null());
        assert!(error.is_null());
        let list_ref = unsafe { &*list };
        let links = unsafe { std::slice::from_raw_parts(list_ref.links, list_ref.len) };
        let clicked: Vec<&[u8]> = links
            .iter()
            .map(|link| &data[link.start as usize..link.end as usize])
            .collect();
        assert_eq!(clicked, [&b"Panel"[..], b"./Panel", b"Panel"]);
        for link in links {
            let specifier = &data[link.specifier_start as usize..link.specifier_end as usize];
            assert_eq!(specifier, b"./Panel");
        }
        unsafe { rcgit_import_link_list_free(list) };

        let rust = CString::new("main.rs").unwrap();
        let list =
            unsafe { rcgit_import_links(rust.as_ptr(), data.as_ptr(), data.len(), &raw mut error) };
        assert!(!list.is_null(), "a language without links is not an error");
        assert_eq!(unsafe { &*list }.len, 0);
        assert!(error.is_null());
        unsafe { rcgit_import_link_list_free(list) };

        let list = unsafe { rcgit_import_links(name.as_ptr(), ptr::null(), 0, &raw mut error) };
        assert!(!list.is_null(), "no bytes means no links");
        assert_eq!(unsafe { &*list }.len, 0);
        unsafe { rcgit_import_link_list_free(list) };

        let list = unsafe { rcgit_import_links(name.as_ptr(), ptr::null(), 5, &raw mut error) };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "rcgit_import_links: data is NULL");

        let unknown = CString::new("photo.png").unwrap();
        let list = unsafe {
            rcgit_import_links(unknown.as_ptr(), data.as_ptr(), data.len(), &raw mut error)
        };
        assert!(list.is_null());
        assert!(error.is_null(), "an unknown language is not an error");

        let list = unsafe { rcgit_import_links(ptr::null(), data.as_ptr(), 0, &raw mut error) };
        assert!(list.is_null());
        assert_eq!(take_error(&mut error), "rcgit_import_links: name is NULL");
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
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_branch_create: repo_path is NULL"
        );
    }

    #[test]
    fn the_default_branch_crosses_as_a_string_or_null() {
        let fx = Fixture::standard();
        let repo = open(&fx);
        let mut error = ptr::null_mut();
        let branch = unsafe { rcgit_default_branch(repo, &raw mut error) };
        assert!(error.is_null(), "success must leave error untouched");
        assert_eq!(take_string(branch), "main");
        unsafe { rcgit_repo_free(repo) };

        let bare = Fixture::empty_dir();
        bare.git(&["init", "-q", "-b", "work"]);
        let repo = open(&bare);
        let branch = unsafe { rcgit_default_branch(repo, &raw mut error) };
        assert!(branch.is_null(), "nothing names a default branch");
        assert!(error.is_null(), "and that is not a failure");
        unsafe { rcgit_repo_free(repo) };

        assert!(unsafe { rcgit_default_branch(ptr::null(), &raw mut error) }.is_null());
        assert_eq!(take_error(&mut error), "rcgit_default_branch: repo is NULL");
    }

    #[test]
    fn checkout_switches_the_branch_and_reports_null_input() {
        let fx = Fixture::standard();
        let path = CString::new(fx.path().as_os_str().as_bytes()).unwrap();
        let branch = CString::new("feature").unwrap();
        let mut out = RcGitCheckoutOutcome {
            new_head: RcGitOid::empty(),
            files_changed: 0,
            already_on_branch: false,
        };
        let mut error = ptr::null_mut();
        let status = unsafe {
            rcgit_guard_checkout(path.as_ptr(), branch.as_ptr(), &raw mut out, &raw mut error)
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert!(error.is_null(), "success must leave error untouched");
        assert!(!out.already_on_branch);
        assert!(out.files_changed > 0);
        assert_eq!(hex(&out.new_head), fx.rev_parse("feature"));
        assert_eq!(
            fx.git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
            "feature"
        );

        // Already there: nothing is written and the outcome says so.
        let status = unsafe {
            rcgit_guard_checkout(path.as_ptr(), branch.as_ptr(), &raw mut out, &raw mut error)
        };
        assert_eq!(status, RCGIT_OK, "{}", take_error(&mut error));
        assert!(out.already_on_branch);
        assert_eq!(out.files_changed, 0);

        let missing = CString::new("no-such-branch").unwrap();
        let status = unsafe {
            rcgit_guard_checkout(
                path.as_ptr(),
                missing.as_ptr(),
                &raw mut out,
                &raw mut error,
            )
        };
        assert_eq!(status, RCGIT_ERROR);
        assert!(take_error(&mut error).contains("no-such-branch"));

        let status = unsafe {
            rcgit_guard_checkout(ptr::null(), branch.as_ptr(), &raw mut out, &raw mut error)
        };
        assert_eq!(status, RCGIT_ERROR);
        assert_eq!(
            take_error(&mut error),
            "rcgit_guard_checkout: repo_path is NULL"
        );
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
