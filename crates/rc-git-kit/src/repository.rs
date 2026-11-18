//! Port of `RepositoryReading.swift`, with the behaviour of `Repository.swift` and
//! `RepositoryError.swift`: the read operations the models depend on, as traits a test fake
//! can stand in for, and the live implementation over [`rc_git_core::Repository`].
//!
//! The Swift files were mostly FFI marshalling; what survives is the error shape (one message
//! that already names the operation and the object) and the scan cap. Core's types cross
//! unchanged: `Oid`, `TreeEntry`, `BlobContent` and the rest. Read limits are clamped by core
//! itself ([`rc_git_core::clamp_read_limit`]), so nothing here clamps again.
//!
//! Every method blocks. Call them through [`crate::blocking::blocking`], never on the UI
//! thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rc_git_core::{
    Blame, BlobContent, BlobDiff, CommitDetail, CommitFilter, CommitPage, CommitSummary,
    ContentQuery, DefaultBranch, Head, HistoryWalk, LogCursor, MergeBase, Oid, RefListing,
    SearchBatch, SearchLimits, StashEntry, StatusReport, TreeChange, TreeEntry, WorktreeFileList,
    WorktreeListing,
};

/// The cap a full working-tree status scan stops at, matching the path-search cap.
pub const DEFAULT_STATUS_SCAN_CAP: usize = 200_000;

/// A failed read. The message already names the operation and the object it failed on, as
/// core's errors do; a fake builds one from any text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
#[error("{message}")]
pub struct RepositoryError {
    message: String,
}

impl RepositoryError {
    /// An error carrying `message` as it should be shown.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The message to show.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<rc_git_core::Error> for RepositoryError {
    fn from(error: rc_git_core::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// What every read returns.
pub type ReadResult<T> = Result<T, RepositoryError>;

/// The read operations the models depend on. [`LiveRepository`] is the real implementation;
/// tests inject a fake. Every method blocks and may be called from any thread.
pub trait RepositoryReading: Send + Sync {
    /// The working directory, or `None` for a bare repository.
    fn work_dir(&self) -> Option<PathBuf>;

    /// Where `HEAD` points.
    ///
    /// # Errors
    /// When `HEAD` cannot be read.
    fn head(&self) -> ReadResult<Head>;

    /// The repository's mainline branch, which Back to default switches to; `None` when
    /// nothing names one.
    ///
    /// # Errors
    /// When the refs that name it cannot be read.
    fn default_branch(&self) -> ReadResult<Option<DefaultBranch>>;

    /// Every reference, plus the ones whose target could not be read.
    ///
    /// # Errors
    /// When the ref store cannot be listed.
    fn refs(&self) -> ReadResult<RefListing>;

    /// The object `spec` names.
    ///
    /// # Errors
    /// When `spec` is not a revision in this repository.
    fn resolve(&self, spec: &str) -> ReadResult<Oid>;

    /// The tree `spec` names, peeling commits and tags.
    ///
    /// # Errors
    /// When `spec` names nothing or nothing that peels to a tree.
    fn resolve_tree(&self, spec: &str) -> ReadResult<Oid>;

    /// The tree of commit or tag `oid`, or `oid` itself when it is a tree.
    ///
    /// # Errors
    /// When `oid` is missing or a blob.
    fn tree_of(&self, oid: Oid) -> ReadResult<Oid>;

    /// The children of `tree`, in display order.
    ///
    /// # Errors
    /// When `tree` is missing or not a tree.
    fn tree_entries(&self, tree: Oid) -> ReadResult<Vec<TreeEntry>>;

    /// A blob for display, read only when its header says it fits `limit`.
    ///
    /// # Errors
    /// When `oid` is missing or not a blob.
    fn read_blob(&self, oid: Oid, limit: u64) -> ReadResult<BlobContent>;

    /// A walk over the commits reachable from `tip`, positioned before the newest.
    ///
    /// # Errors
    /// When `tip` is missing or not a commit.
    fn log_cursor(&self, tip: Oid) -> ReadResult<Box<dyn LogCursorReading>>;

    /// One commit's list-view parts.
    ///
    /// # Errors
    /// When `oid` is missing or not a commit.
    fn commit(&self, oid: Oid) -> ReadResult<CommitSummary>;

    /// One commit's detail-view parts.
    ///
    /// # Errors
    /// When `oid` is missing or not a commit.
    fn commit_details(&self, oid: Oid) -> ReadResult<CommitDetail>;

    /// A filtered walk over the commits reachable from `tip`, positioned before the newest.
    ///
    /// # Errors
    /// When `tip` is missing or not a commit, or the filter's path cannot be looked up.
    fn history_walk(
        &self,
        tip: Oid,
        filter: CommitFilter,
    ) -> ReadResult<Box<dyn HistoryWalkReading>>;

    /// The changed files between two trees; `from: None` diffs against the empty tree.
    ///
    /// # Errors
    /// When either side is missing or not a tree.
    fn diff_trees(&self, from: Option<Oid>, to: Oid) -> ReadResult<Vec<TreeChange>>;

    /// Whether the object database holds `oid`.
    fn has_object(&self, oid: Oid) -> bool;

    /// The merge base of two commits, reading at most `max_scan` commits.
    ///
    /// # Errors
    /// When either side, or a commit on the way, is missing or not a commit.
    fn merge_base(&self, one: Oid, two: Oid, max_scan: usize) -> ReadResult<MergeBase>;

    /// The line diff of two blobs; `None` stands for an absent side.
    ///
    /// # Errors
    /// When a side is missing or not a blob.
    fn diff_blobs(
        &self,
        old: Option<Oid>,
        new: Option<Oid>,
        limit: u64,
        context: u32,
    ) -> ReadResult<BlobDiff>;

    /// Who last changed each line of `path` at `tip`, stopping at the `since` floor.
    ///
    /// # Errors
    /// When `path` does not exist at `tip` or the walk cannot read a commit.
    fn blame(&self, tip: Oid, path: &str, limit: u64, since: Option<i64>) -> ReadResult<Blame>;

    /// One working-tree directory against the same directory of `tree`.
    ///
    /// # Errors
    /// When the repository is bare or the directory cannot be read.
    fn worktree_entries(&self, tree: Option<Oid>, dir: &str) -> ReadResult<WorktreeListing>;

    /// A working-tree file for display, read only when it fits `limit`.
    ///
    /// # Errors
    /// When the repository is bare or the file cannot be read.
    fn worktree_file(&self, path: &str, limit: u64) -> ReadResult<BlobContent>;

    /// Every changed path in the working tree against `tree`, stopping at `max_scan` entries.
    ///
    /// # Errors
    /// When the repository is bare or the index cannot be read.
    fn worktree_status(&self, tree: Option<Oid>, max_scan: usize) -> ReadResult<StatusReport>;

    /// Every file on disk, ignored ones too, stopping at `max_scan` entries.
    ///
    /// # Errors
    /// When the repository is bare.
    fn worktree_files(&self, tree: Option<Oid>, max_scan: usize) -> ReadResult<WorktreeFileList>;

    /// The line diff of a working-tree file against blob `old`.
    ///
    /// # Errors
    /// When the file cannot be read or `old` is not a blob.
    fn diff_worktree_file(
        &self,
        path: &str,
        old: Option<Oid>,
        limit: u64,
        context: u32,
    ) -> ReadResult<BlobDiff>;

    /// Every stash entry, newest first.
    ///
    /// # Errors
    /// When the stash reflog cannot be read.
    fn stash_list(&self) -> ReadResult<Vec<StashEntry>>;

    /// Searches blobs in order until the byte budget is spent; fewer results than `oids`
    /// means ask again for the rest.
    ///
    /// # Errors
    /// When a blob is missing or not a blob.
    fn search_blobs(
        &self,
        oids: &[Oid],
        query: &ContentQuery,
        limits: SearchLimits,
    ) -> ReadResult<SearchBatch>;

    /// Searches working-tree files in order until the byte budget is spent; a file that
    /// cannot be read is reported as unreadable rather than failing the call.
    ///
    /// # Errors
    /// When the repository is bare.
    fn search_worktree_files(
        &self,
        paths: &[String],
        query: &ContentQuery,
        limits: SearchLimits,
    ) -> ReadResult<SearchBatch>;
}

/// A walk over the commits reachable from one tip, newest first. Each call resumes where the
/// previous one stopped, so paging to depth N costs N commits in total.
pub trait LogCursorReading: Send + Sync {
    /// The next at most `limit` commits; fewer only at the end of history. After a failed call
    /// every later one fails too: open a new cursor.
    ///
    /// # Errors
    /// When the walk cannot advance or a commit cannot be read.
    fn next(&self, limit: usize) -> ReadResult<Vec<CommitSummary>>;
}

/// A filtered walk over the commits reachable from one tip, newest first. Each call resumes
/// where the previous one stopped, so a sparse filter costs one pass over history in total.
pub trait HistoryWalkReading: Send + Sync {
    /// At most `limit` matches from at most `max_scan` commits examined; loop until the page
    /// is complete. After a failed call every later one fails too: open a new walk.
    ///
    /// # Errors
    /// When the walk cannot advance or a commit cannot be read.
    fn next(&self, limit: usize, max_scan: usize) -> ReadResult<CommitPage>;
}

/// An open repository backed by the core. Cheap to clone and safe from any thread: the core
/// keeps a pool of handles and never blocks one call on another.
#[derive(Debug, Clone)]
pub struct LiveRepository {
    repository: Arc<rc_git_core::Repository>,
}

impl LiveRepository {
    /// Opens a working directory or a `.git` directory.
    ///
    /// # Errors
    /// When `path` is not a repository rc-git can read; the message names the path.
    pub fn open(path: impl AsRef<Path>) -> ReadResult<Self> {
        let repository = rc_git_core::Repository::open(path)?;
        Ok(Self {
            repository: Arc::new(repository),
        })
    }

    /// This worktree's git directory: `.git`, or `.git/worktrees/<name>` for a linked
    /// worktree.
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        self.repository.git_dir()
    }

    /// The git directory every worktree shares, where branches, tags and `packed-refs` live.
    /// Equal to [`Self::git_dir`] except in a linked worktree.
    #[must_use]
    pub fn common_dir(&self) -> &Path {
        self.repository.common_dir()
    }
}

impl RepositoryReading for LiveRepository {
    fn work_dir(&self) -> Option<PathBuf> {
        self.repository.work_dir().map(Path::to_path_buf)
    }

    fn head(&self) -> ReadResult<Head> {
        Ok(self.repository.head()?)
    }

    fn default_branch(&self) -> ReadResult<Option<DefaultBranch>> {
        Ok(self.repository.default_branch()?)
    }

    fn refs(&self) -> ReadResult<RefListing> {
        Ok(self.repository.refs()?)
    }

    fn resolve(&self, spec: &str) -> ReadResult<Oid> {
        Ok(self.repository.resolve(spec)?)
    }

    fn resolve_tree(&self, spec: &str) -> ReadResult<Oid> {
        Ok(self.repository.resolve_tree(spec)?)
    }

    fn tree_of(&self, oid: Oid) -> ReadResult<Oid> {
        Ok(self.repository.tree_of(oid)?)
    }

    fn tree_entries(&self, tree: Oid) -> ReadResult<Vec<TreeEntry>> {
        Ok(self.repository.tree_entries(tree)?)
    }

    fn read_blob(&self, oid: Oid, limit: u64) -> ReadResult<BlobContent> {
        Ok(self.repository.read_blob(oid, limit)?)
    }

    fn log_cursor(&self, tip: Oid) -> ReadResult<Box<dyn LogCursorReading>> {
        let cursor = self.repository.log_cursor(tip)?;
        Ok(Box::new(LiveLogCursor {
            repository: Arc::clone(&self.repository),
            cursor,
        }))
    }

    fn commit(&self, oid: Oid) -> ReadResult<CommitSummary> {
        Ok(self.repository.commit(oid)?)
    }

    fn commit_details(&self, oid: Oid) -> ReadResult<CommitDetail> {
        Ok(self.repository.commit_details(oid)?)
    }

    fn history_walk(
        &self,
        tip: Oid,
        filter: CommitFilter,
    ) -> ReadResult<Box<dyn HistoryWalkReading>> {
        let walk = self.repository.history_walk(tip, filter)?;
        Ok(Box::new(LiveHistoryWalk {
            repository: Arc::clone(&self.repository),
            walk,
        }))
    }

    fn diff_trees(&self, from: Option<Oid>, to: Oid) -> ReadResult<Vec<TreeChange>> {
        Ok(self.repository.diff_trees(from, to)?)
    }

    fn has_object(&self, oid: Oid) -> bool {
        self.repository.has_object(oid)
    }

    fn merge_base(&self, one: Oid, two: Oid, max_scan: usize) -> ReadResult<MergeBase> {
        Ok(self.repository.merge_base(one, two, max_scan)?)
    }

    fn diff_blobs(
        &self,
        old: Option<Oid>,
        new: Option<Oid>,
        limit: u64,
        context: u32,
    ) -> ReadResult<BlobDiff> {
        Ok(self.repository.diff_blobs(old, new, limit, context)?)
    }

    fn blame(&self, tip: Oid, path: &str, limit: u64, since: Option<i64>) -> ReadResult<Blame> {
        Ok(self.repository.blame(tip, path, limit, since)?)
    }

    fn worktree_entries(&self, tree: Option<Oid>, dir: &str) -> ReadResult<WorktreeListing> {
        Ok(self.repository.worktree_entries(tree, dir)?)
    }

    fn worktree_file(&self, path: &str, limit: u64) -> ReadResult<BlobContent> {
        Ok(self.repository.worktree_file(path, limit)?)
    }

    fn worktree_status(&self, tree: Option<Oid>, max_scan: usize) -> ReadResult<StatusReport> {
        Ok(self.repository.worktree_status(tree, max_scan)?)
    }

    fn worktree_files(&self, tree: Option<Oid>, max_scan: usize) -> ReadResult<WorktreeFileList> {
        Ok(self.repository.worktree_files(tree, max_scan)?)
    }

    fn diff_worktree_file(
        &self,
        path: &str,
        old: Option<Oid>,
        limit: u64,
        context: u32,
    ) -> ReadResult<BlobDiff> {
        Ok(self
            .repository
            .diff_worktree_file(old, path, limit, context)?)
    }

    fn stash_list(&self) -> ReadResult<Vec<StashEntry>> {
        Ok(self.repository.stash_list()?)
    }

    fn search_blobs(
        &self,
        oids: &[Oid],
        query: &ContentQuery,
        limits: SearchLimits,
    ) -> ReadResult<SearchBatch> {
        Ok(self.repository.search_blobs(oids, query, limits)?)
    }

    fn search_worktree_files(
        &self,
        paths: &[String],
        query: &ContentQuery,
        limits: SearchLimits,
    ) -> ReadResult<SearchBatch> {
        Ok(self
            .repository
            .search_worktree_files(paths, query, limits)?)
    }
}

/// A core log cursor and the repository that made it, which every page needs.
struct LiveLogCursor {
    repository: Arc<rc_git_core::Repository>,
    cursor: LogCursor,
}

impl LogCursorReading for LiveLogCursor {
    fn next(&self, limit: usize) -> ReadResult<Vec<CommitSummary>> {
        Ok(self.cursor.next_page(&self.repository, limit)?)
    }
}

/// A core filtered walk and the repository that made it.
struct LiveHistoryWalk {
    repository: Arc<rc_git_core::Repository>,
    walk: HistoryWalk,
}

impl HistoryWalkReading for LiveHistoryWalk {
    fn next(&self, limit: usize, max_scan: usize) -> ReadResult<CommitPage> {
        Ok(self.walk.next_page(&self.repository, limit, max_scan)?)
    }
}
