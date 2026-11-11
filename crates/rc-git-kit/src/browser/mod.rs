//! Port of `BrowserModel.swift`: the state of one browser tab as a GPUI entity. It holds the
//! open repository, its refs, the browsed tree, the file in the file pane, the paged history,
//! the commit detail, blame, and the indexes behind Go to File and Find in Repository.
//!
//! Every asynchronous load carries a generation number. When the user moves on before a load
//! finishes, the stale result is dropped instead of overwriting the newer state.
//!
//! Each public operation runs its first step synchronously, inside the update that called it,
//! and the rest as a detached task. The returned [`Task`] only waits for the end, so dropping
//! it never stops an operation half way: a move that began always ends, and a restore or
//! refresh that started always releases its queue.
//!
//! The model is split by pane: `refs` (attach, refresh, select), `files` (the file pane and
//! the working-tree browse), `history` (paging and path scope), `search` (commit search),
//! `detail` (commit detail and diff), `blame`, `location` (back/forward and restore),
//! `imports`, `paths` (Go to File) and `content` (Find in Repository).

mod blame;
mod content;
mod detail;
mod files;
mod history;
mod imports;
mod location;
mod paths;
mod refs;
mod search;

use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{AppContext, AsyncApp, Context, Entity, EventEmitter, Task, WeakEntity};
use rc_git_core::{
    CommitFilter, CommitSummary, DEFAULT_BLOB_LIMIT, Head, Oid, RefEntry, RefListing,
};

use crate::back_forward::{BrowserLocation, HistoryScope};
use crate::commit_graph::GraphBuilder;
use crate::highlight_store::HighlightStore;
use crate::history_paging::{HistoryPager, LogPager};
use crate::models::{BlameState, FileDiffState};
use crate::oneshot;
use crate::path_search::PathIndex;
use crate::path_search_session::PathSearchError;
use crate::platform::stores::{HiddenFilesPreference, SharedSettings};
use crate::recent_cache::RecentCache;
use crate::repository::RepositoryReading;
use crate::text_document::FileReveal;
use crate::tree_placeholder;
use crate::tree_store::TreeStore;

pub use blame::{BLAME_WINDOW, blame_floor, blame_refusal, blame_title, blaming_title};
pub use content::ContentSearchError;
pub use detail::CommitDetailState;
pub use files::{
    BlobOpen, FileEntry, FileState, OpenPathOutcome, WorktreeFileOpen, rows, state_for,
};
pub use paths::DEFAULT_PATH_SEARCH_LIMIT;
pub use refs::initial_ref;

/// How many commits one history page fetches.
pub const HISTORY_PAGE_SIZE: usize = 100;
/// How many commits one repository call may examine while filtering by path.
pub const PATH_HISTORY_SCAN_CHUNK: usize = 5_000;
/// How many commits the first repository call of a search page may examine. Each later call
/// doubles it up to [`SEARCH_SCAN_CHUNK`], so a query replaced while typing stops after a short
/// walk instead of holding a thread for a full chunk.
pub const SEARCH_FIRST_SCAN_CHUNK: usize = 500;
/// How many commits one repository call may examine while searching.
pub const SEARCH_SCAN_CHUNK: usize = 10_000;
/// How many tips (and tip-and-path scopes) keep their walked history pages.
pub const HISTORY_CACHE_LIMIT: NonZeroUsize = NonZeroUsize::MIN.saturating_add(3);
/// How many roots keep their path index; one index runs to tens of megabytes.
pub const PATH_INDEX_CACHE_LIMIT: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

/// What changed, so a view re-renders only its own slice. Every event also notifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserEvent {
    /// Refs, `HEAD`, the selection, or the stale flag changed.
    RefsChanged,
    /// The root tree changed; the tree view reloads from scratch.
    RootChanged,
    /// The file pane's state changed.
    FileChanged,
    /// The history list, its scope, paging or error changed.
    HistoryChanged,
    /// The search filter, results, progress or error changed.
    SearchChanged,
    /// The commit detail, its changes or the file diff changed.
    DiffChanged,
    /// The blame state changed.
    BlameChanged,
    /// The location changed, once per move: a ref switch that also closes the open file
    /// reports where it ended up, never the step in between.
    LocationChanged,
    /// A background operation failed with a message worth showing.
    Error(String),
}

/// Where the open file's bytes came from: a working-tree file read by path, or a blob.
#[derive(Debug, Clone)]
enum OpenedFileSource {
    /// Read from disk; `status` is how the file relates to `HEAD`, `None` when the opener did
    /// not know.
    Disk {
        name: String,
        status: Option<rc_git_core::WorktreeStatus>,
    },
    Blob(FileEntry),
}

/// One counter per load stream. A load records its stream's value when it starts and applies
/// its result only while the value is unchanged.
#[derive(Debug, Default)]
struct Generations {
    root: u64,
    file: u64,
    refresh: u64,
    history: u64,
    diff: u64,
    search: u64,
    blame: u64,
    worktree_index: u64,
    path_search: u64,
    content_search: u64,
}

/// The unfiltered pages of one tip plus the lane graph built alongside them, and the cursor
/// that stands after them, so returning to the tip resumes instead of re-walking.
#[derive(Debug, Clone)]
struct HistoryPages {
    commits: Arc<Vec<CommitSummary>>,
    complete: bool,
    graph: Arc<GraphBuilder>,
    pager: Option<Arc<LogPager>>,
}

/// The filtered pages of one tip and path, and how far the walk behind them examined.
#[derive(Debug, Clone)]
struct PathPages {
    commits: Arc<Vec<CommitSummary>>,
    scanned: usize,
    complete: bool,
}

/// One cache key of path-scoped history.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PathHistoryKey {
    tip: Oid,
    path: String,
}

/// What [`BrowserModel::path_index`] hands back.
type IndexResult = Result<Arc<PathIndex>, PathSearchError>;

/// The path index build running for one root, and the callers waiting for it.
struct IndexBuild {
    id: u64,
    root: Oid,
    cancel: Arc<AtomicBool>,
    waiters: Vec<oneshot::Sender<IndexResult>>,
}

/// The working tree walk running for one generation, and the callers waiting for it.
struct WorktreeBuild {
    generation: u64,
    waiters: Rc<RefCell<Vec<oneshot::Sender<IndexResult>>>>,
}

/// What the sidebar and history show as browsed, kept so a root that cannot be read can hand
/// the selection back to the tree still on screen.
#[derive(Debug, Clone)]
struct Selection {
    reference: Option<RefEntry>,
    commit: Option<CommitSummary>,
    working_tree: bool,
    history_anchor: Option<Oid>,
}

/// One blame walk's inputs, plus the file generation it was asked under.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlameTarget {
    path: String,
    tip: Oid,
    limit: u64,
    file: u64,
    /// True when the tip is `HEAD` standing in for the working tree.
    at_head: bool,
}

/// The state of one browser tab. See the module documentation.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent pane flags, ported one for one from BrowserModel.swift"
)]
pub struct BrowserModel {
    repository: Option<Arc<dyn RepositoryReading>>,
    refs: RefListing,
    head: Option<Head>,
    selected_ref: Option<RefEntry>,
    browsed_commit: Option<CommitSummary>,
    browsing_working_tree: bool,
    root_tree: Option<Oid>,
    file_state: FileState,
    opened_file_path: Option<String>,
    opened_path_is_exact: bool,
    pending_reveal: Option<FileReveal>,
    /// The reveal `open_path` or `open_content_match` hands to the load it is about to start;
    /// the load moves it to `pending_reveal`, so a load started any other way clears that.
    reveal_for_next_load: Option<FileReveal>,
    opened_file_limit: u64,
    opened_file_source: Option<OpenedFileSource>,
    refs_stale: bool,
    shows_hidden_files: bool,
    trees: Arc<TreeStore>,
    highlights: Entity<HighlightStore>,

    diff_commit: Option<CommitSummary>,
    commit_changes: Vec<rc_git_core::TreeChange>,
    file_diff: FileDiffState,
    commit_detail: CommitDetailState,
    blame: BlameState,
    blame_in_flight: Option<BlameTarget>,

    history: Arc<Vec<CommitSummary>>,
    history_complete: bool,
    is_loading_history: bool,
    history_error: Option<String>,
    history_scope: HistoryScope,
    history_anchor: Option<Oid>,
    graph: Arc<GraphBuilder>,
    history_cache: RecentCache<Oid, HistoryPages>,
    path_history_cache: RecentCache<PathHistoryKey, PathPages>,
    /// The path walk of the current scope, for one key and history generation.
    path_pager: Option<(PathHistoryKey, u64, Arc<HistoryPager>)>,

    search_filter: Option<CommitFilter>,
    search_results: Vec<CommitSummary>,
    search_scanned: usize,
    search_complete: bool,
    is_searching: bool,
    search_error: Option<String>,
    search_pager: Option<Arc<HistoryPager>>,
    /// A search asked for while there was no anchor to walk from; it runs once one is set.
    pending_search: Option<CommitFilter>,

    generations: Generations,
    is_restoring: bool,
    pending_restore: Option<BrowserLocation>,
    refresh_running: bool,
    refresh_pending: bool,
    /// Everyone waiting for the running refresh loop to end: joined refreshes and a restore.
    refresh_waiters: Vec<oneshot::Sender<()>>,
    move_depth: usize,
    reported_location: BrowserLocation,

    path_indices: RecentCache<Oid, Arc<PathIndex>>,
    index_build: Option<IndexBuild>,
    next_index_build: u64,
    worktree_index: Option<Arc<PathIndex>>,
    worktree_index_build: Option<WorktreeBuild>,
    /// The cancel flag of the newest path search scan.
    path_search_cancel: Option<Arc<AtomicBool>>,
    path_search_scans: usize,
}

impl EventEmitter<BrowserEvent> for BrowserModel {}

impl BrowserModel {
    /// A model with no repository. Whether hidden files show starts from the saved preference.
    pub fn new(settings: SharedSettings, cx: &mut Context<Self>) -> Self {
        let shows_hidden_files = HiddenFilesPreference::new(settings).shows();
        Self {
            repository: None,
            refs: RefListing::default(),
            head: None,
            selected_ref: None,
            browsed_commit: None,
            browsing_working_tree: false,
            root_tree: None,
            file_state: FileState::Empty,
            opened_file_path: None,
            opened_path_is_exact: true,
            pending_reveal: None,
            reveal_for_next_load: None,
            opened_file_limit: DEFAULT_BLOB_LIMIT,
            opened_file_source: None,
            refs_stale: false,
            shows_hidden_files,
            trees: Arc::new(TreeStore::default()),
            highlights: cx.new(|_| HighlightStore::default()),
            diff_commit: None,
            commit_changes: Vec::new(),
            file_diff: FileDiffState::Empty,
            commit_detail: CommitDetailState::Empty,
            blame: BlameState::Empty,
            blame_in_flight: None,
            history: Arc::default(),
            history_complete: false,
            is_loading_history: false,
            history_error: None,
            history_scope: HistoryScope::All,
            history_anchor: None,
            graph: Arc::default(),
            history_cache: RecentCache::new(HISTORY_CACHE_LIMIT),
            path_history_cache: RecentCache::new(HISTORY_CACHE_LIMIT),
            path_pager: None,
            search_filter: None,
            search_results: Vec::new(),
            search_scanned: 0,
            search_complete: false,
            is_searching: false,
            search_error: None,
            search_pager: None,
            pending_search: None,
            generations: Generations::default(),
            is_restoring: false,
            pending_restore: None,
            refresh_running: false,
            refresh_pending: false,
            refresh_waiters: Vec::new(),
            move_depth: 0,
            reported_location: BrowserLocation::default(),
            path_indices: RecentCache::new(PATH_INDEX_CACHE_LIMIT),
            index_build: None,
            next_index_build: 0,
            worktree_index: None,
            worktree_index_build: None,
            path_search_cancel: None,
            path_search_scans: 0,
        }
    }

    /// The open repository.
    #[must_use]
    pub fn repository(&self) -> Option<&Arc<dyn RepositoryReading>> {
        self.repository.as_ref()
    }

    /// Every ref of the last listing that read.
    #[must_use]
    pub fn refs(&self) -> &RefListing {
        &self.refs
    }

    /// `HEAD` as last read; `None` until a listing loads.
    #[must_use]
    pub fn head(&self) -> Option<&Head> {
        self.head.as_ref()
    }

    /// The ref the tree pane browses.
    #[must_use]
    pub fn selected_ref(&self) -> Option<&RefEntry> {
        self.selected_ref.as_ref()
    }

    /// Set instead of the selected ref while browsing one commit from the history.
    #[must_use]
    pub fn browsed_commit(&self) -> Option<&CommitSummary> {
        self.browsed_commit.as_ref()
    }

    /// Whether the tree pane shows the working directory instead of a committed tree.
    #[must_use]
    pub fn browsing_working_tree(&self) -> bool {
        self.browsing_working_tree
    }

    /// The root tree on screen.
    #[must_use]
    pub fn root_tree(&self) -> Option<Oid> {
        self.root_tree
    }

    /// What the tree says while there is no root tree.
    #[must_use]
    pub fn tree_placeholder(&self) -> &'static str {
        tree_placeholder::text(self.repository.is_some(), self.head.as_ref())
    }

    /// What the file pane shows.
    #[must_use]
    pub fn file_state(&self) -> &FileState {
        &self.file_state
    }

    /// Repository-relative path of the file in the file pane, when the opener knew it.
    #[must_use]
    pub fn opened_file_path(&self) -> Option<&str> {
        self.opened_file_path.as_deref()
    }

    /// Whether every component of the open path is the stored name byte for byte. When it is
    /// not, the path carries U+FFFD replacements, so blame and file history refuse it.
    #[must_use]
    pub fn opened_path_is_exact(&self) -> bool {
        self.opened_path_is_exact
    }

    /// Where the pane scrolls once the file loading or on screen has painted.
    #[must_use]
    pub fn pending_reveal(&self) -> Option<&FileReveal> {
        self.pending_reveal.as_ref()
    }

    /// True after a ref refresh failed: the refs and selection are the last listing that read.
    #[must_use]
    pub fn refs_stale(&self) -> bool {
        self.refs_stale
    }

    /// Whether the file lists show entries [`crate::hidden_files`] names.
    #[must_use]
    pub fn shows_hidden_files(&self) -> bool {
        self.shows_hidden_files
    }

    /// The directory listings, shared with the tree view.
    #[must_use]
    pub fn trees(&self) -> &Arc<TreeStore> {
        &self.trees
    }

    /// The highlight and import-link caches. The model stops their running parses whenever
    /// the open file changes.
    #[must_use]
    pub fn highlight_store(&self) -> &Entity<HighlightStore> {
        &self.highlights
    }

    /// The commit whose changed files the history detail shows.
    #[must_use]
    pub fn diff_commit(&self) -> Option<&CommitSummary> {
        self.diff_commit.as_ref()
    }

    /// Files changed by the detail commit against its first parent.
    #[must_use]
    pub fn commit_changes(&self) -> &[rc_git_core::TreeChange] {
        &self.commit_changes
    }

    /// The line diff of one changed file.
    #[must_use]
    pub fn file_diff(&self) -> &FileDiffState {
        &self.file_diff
    }

    /// The detail commit's message, committer and refs.
    #[must_use]
    pub fn commit_detail(&self) -> &CommitDetailState {
        &self.commit_detail
    }

    /// The blame last started or finished for the open file.
    #[must_use]
    pub fn blame(&self) -> &BlameState {
        &self.blame
    }

    /// Commits loaded so far, newest first.
    #[must_use]
    pub fn history(&self) -> &[CommitSummary] {
        &self.history
    }

    /// True once a page came back short, or a filtered walk ended.
    #[must_use]
    pub fn history_complete(&self) -> bool {
        self.history_complete
    }

    /// Whether a history page is loading.
    #[must_use]
    pub fn is_loading_history(&self) -> bool {
        self.is_loading_history
    }

    /// Why the last history page failed. Paging refuses while it is set.
    #[must_use]
    pub fn history_error(&self) -> Option<&str> {
        self.history_error.as_deref()
    }

    /// Whether the history pane shows everything or one path's commits.
    #[must_use]
    pub fn history_scope(&self) -> &HistoryScope {
        &self.history_scope
    }

    /// The active search, or `None` while the pane shows the plain history.
    #[must_use]
    pub fn search_filter(&self) -> Option<&CommitFilter> {
        self.search_filter.as_ref()
    }

    /// Matches found so far, newest first.
    #[must_use]
    pub fn search_results(&self) -> &[CommitSummary] {
        &self.search_results
    }

    /// How many commits the search has examined.
    #[must_use]
    pub fn search_scanned(&self) -> usize {
        self.search_scanned
    }

    /// True when the search walked to the end of history.
    #[must_use]
    pub fn search_complete(&self) -> bool {
        self.search_complete
    }

    /// Whether a search walk is running.
    #[must_use]
    pub fn is_searching(&self) -> bool {
        self.is_searching
    }

    /// Why the active search stopped. Paging refuses while it is set.
    #[must_use]
    pub fn search_error(&self) -> Option<&str> {
        self.search_error.as_deref()
    }

    /// Tips whose unfiltered pages are cached; for tests of the bound.
    #[must_use]
    pub fn history_cache_count(&self) -> usize {
        self.history_cache.len()
    }

    /// Tip-and-path scopes whose pages are cached; for tests of the bound.
    #[must_use]
    pub fn path_history_cache_count(&self) -> usize {
        self.path_history_cache.len()
    }

    /// Roots whose path index is cached; for tests of the bound.
    #[must_use]
    pub fn path_index_cache_count(&self) -> usize {
        self.path_indices.len()
    }

    /// Index scans path search started; for tests of the supersession.
    #[must_use]
    pub fn path_search_scans(&self) -> usize {
        self.path_search_scans
    }

    /// Sends `event` and notifies observers.
    fn changed(event: BrowserEvent, cx: &mut Context<Self>) {
        cx.emit(event);
        cx.notify();
    }

    /// Shows `message` through [`BrowserEvent::Error`]. The app logs it.
    fn fail(message: impl Into<String>, cx: &mut Context<Self>) {
        cx.emit(BrowserEvent::Error(message.into()));
    }

    /// Bumps the file generation, which also stops the parses for the file that was open.
    fn bump_file_generation(&mut self, cx: &mut Context<Self>) -> u64 {
        self.generations.file += 1;
        self.highlights
            .update(cx, |store, _| store.cancel_running());
        self.generations.file
    }

    fn current_selection(&self) -> Selection {
        Selection {
            reference: self.selected_ref.clone(),
            commit: self.browsed_commit.clone(),
            working_tree: self.browsing_working_tree,
            history_anchor: self.history_anchor,
        }
    }

    /// Runs `work` as a detached task, so it runs to its end whatever happens to the handle,
    /// and returns a task that waits for its value; `fallback` when the model went away.
    fn run<R: 'static>(
        cx: &mut Context<Self>,
        fallback: R,
        work: impl AsyncFnOnce(WeakEntity<Self>, &mut AsyncApp) -> R + 'static,
    ) -> Task<R> {
        let (sender, receiver) = oneshot::channel();
        cx.spawn(async move |this, cx| sender.send(work(this, cx).await))
            .detach();
        cx.spawn(async move |_, _| receiver.await.unwrap_or(fallback))
    }
}

/// Runs `step` on the model, `None` once it is gone.
fn with<R>(
    this: &WeakEntity<BrowserModel>,
    cx: &mut AsyncApp,
    step: impl FnOnce(&mut BrowserModel, &mut Context<BrowserModel>) -> R,
) -> Option<R> {
    this.update(cx, step).ok()
}

/// Starts `operation` on the model and waits for it, `None` once the model is gone.
async fn call<R: 'static>(
    this: &WeakEntity<BrowserModel>,
    cx: &mut AsyncApp,
    operation: impl FnOnce(&mut BrowserModel, &mut Context<BrowserModel>) -> Task<R>,
) -> Option<R> {
    let task = this.update(cx, operation).ok()?;
    Some(task.await)
}

/// Git's null id, twenty zero bytes, standing in for the blob of a disk entry the compared
/// tree does not have. Nothing reads it: such a row opens from disk, and its directory lists
/// by path.
fn null_oid() -> Option<Oid> {
    Oid::from_bytes(&[0; 20])
}

/// Whether `filter` would match every commit.
fn filter_is_empty(filter: &CommitFilter) -> bool {
    let CommitFilter {
        message,
        author,
        after,
        before,
        path,
    } = filter;
    message.is_none() && author.is_none() && after.is_none() && before.is_none() && path.is_none()
}
