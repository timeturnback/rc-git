//! Port of `FakeRepository.swift`: an in-memory [`RepositoryReading`] for model tests.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use rc_git_core::{
    Blame, BlobContent, BlobDiff, CommitDetail, CommitFilter, CommitPage, CommitSummary,
    ContentQuery, DefaultBranch, FileSearch, Head, MergeBase, Oid, RefListing, SearchBatch,
    SearchLimits, StashEntry, StatusReport, TreeChange, TreeEntry, WorktreeFileList,
    WorktreeListing,
};

use super::CallGate;
use crate::repository::{
    HistoryWalkReading, LogCursorReading, ReadResult, RepositoryError, RepositoryReading,
};

/// Everything the fake answers with and every call it counted. Tests set the answers before
/// the call and read the counters after; lock it through [`FakeRepository::state`].
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one fail-next flag per method, as the Swift fake has"
)]
pub struct FakeState {
    /// Listings per tree id.
    pub trees: HashMap<Oid, Vec<TreeEntry>>,
    /// Blob contents per id.
    pub blobs: HashMap<Oid, BlobContent>,
    /// Per oid, the size its header claims, so a test can stand for a blob too big to build.
    pub blob_sizes: HashMap<Oid, u64>,
    /// `read_blob` calls.
    pub read_blob_calls: usize,
    /// Per tip, its history newest first.
    pub commits: HashMap<Oid, Vec<CommitSummary>>,
    /// Single commits `commit` answers before searching `commits`.
    pub summaries: HashMap<Oid, CommitSummary>,
    /// Details `commit_details` answers; others are built from the summary.
    pub details: HashMap<Oid, CommitDetail>,
    /// Per tip and path, the full matching history. Every stored commit counts as one scanned
    /// commit, so a path walk's position indexes into this list.
    pub path_histories: HashMap<(Oid, String), Vec<CommitSummary>>,
    /// Pages taken from path walks, across every walk.
    pub path_history_calls: usize,
    /// When set, a path walk page fails once its walk stands at or past this index.
    pub path_history_fail_from: Option<usize>,
    /// When set, a path walk page returns at most this many matches.
    pub path_matches_per_call: Option<usize>,
    /// How long each path walk page takes, slept outside the lock.
    pub path_history_delay: Duration,
    /// What `resolve` answers per spec; `resolve_tree` maps the answer through `tree_of`.
    pub resolves: HashMap<String, Oid>,
    /// Per spec, holds the next `resolve` call for it, then clears itself.
    pub resolve_gates: HashMap<String, Arc<CallGate>>,
    /// Per oid, holds the next `commit` call for it, then clears itself.
    pub commit_gates: HashMap<Oid, Arc<CallGate>>,
    /// Pages taken from filtered (non-path) walks.
    pub search_calls: usize,
    /// The `max_scan` each filtered page passed, in call order.
    pub search_max_scans: Vec<usize>,
    /// Holds the next filtered page, then clears itself.
    pub search_gate: Option<Arc<CallGate>>,
    /// Pages taken from log cursors.
    pub log_calls: usize,
    /// Tree diffs per new side.
    pub tree_diffs: HashMap<Oid, Vec<TreeChange>>,
    /// The `(from, to)` of every `diff_trees` call, in call order.
    pub diff_tree_calls: Vec<(Option<Oid>, Oid)>,
    /// The objects `has_object` answers present.
    pub objects: HashSet<Oid>,
    /// What `merge_base` answers per `(one, two)`; any other pair fails as a missing commit.
    pub merge_bases: HashMap<(Oid, Oid), MergeBase>,
    /// The `(one, two, max_scan)` of every `merge_base` call, in call order.
    pub merge_base_calls: Vec<(Oid, Oid, usize)>,
    /// Blob diffs per (old, new).
    pub blob_diffs: HashMap<(Option<Oid>, Option<Oid>), BlobDiff>,
    /// `diff_blobs` calls.
    pub diff_blobs_calls: usize,
    /// Blames per path.
    pub blames: HashMap<String, Blame>,
    /// The limit each `blame` call passed, in call order.
    pub blame_limits: Vec<u64>,
    /// The tip each `blame` call passed, in call order.
    pub blame_tips: Vec<Oid>,
    /// The `since` floor each `blame` call passed, in call order.
    pub blame_floors: Vec<Option<i64>>,
    /// Holds the next `blame` call, then clears itself.
    pub blame_gate: Option<Arc<CallGate>>,
    /// Working-tree listings per directory.
    pub worktree_listings: HashMap<String, WorktreeListing>,
    /// Working-tree file contents per path.
    pub worktree_contents: HashMap<String, BlobContent>,
    /// `worktree_file` calls.
    pub worktree_content_calls: usize,
    /// The full status walk result, replaced by tests as the "disk" changes.
    pub status_report: StatusReport,
    /// `worktree_status` calls.
    pub status_calls: usize,
    /// The tree each `worktree_status` call compared against, in call order.
    pub status_trees: Vec<Option<Oid>>,
    /// Fails the next `worktree_status` call.
    pub fail_next_status: bool,
    /// Holds the next `worktree_status` call, then clears itself.
    pub status_gate: Option<Arc<CallGate>>,
    /// Per path, the working-tree-against-blob diff.
    pub worktree_diffs: HashMap<String, BlobDiff>,
    /// Per path, how many `diff_worktree_file` calls read it.
    pub worktree_diff_calls: HashMap<String, usize>,
    /// Per path, holds the next `diff_worktree_file` call for it, then clears itself.
    pub worktree_diff_gates: HashMap<String, Arc<CallGate>>,
    /// The stash list.
    pub stashes: Vec<StashEntry>,
    /// `stash_list` calls.
    pub stash_calls: usize,
    /// Holds the next `stash_list` call, then clears itself.
    pub stash_gate: Option<Arc<CallGate>>,
    /// Fails the next `stash_list` call.
    pub fail_next_stash: bool,
    /// Set to make the fake a checkout rather than a bare repository.
    pub work_dir: Option<PathBuf>,
    /// The full working-tree file walk result.
    pub worktree_file_list: WorktreeFileList,
    /// `worktree_files` calls.
    pub worktree_list_calls: usize,
    /// How long each `worktree_files` call takes.
    pub worktree_list_delay: Duration,
    /// Fails the next `worktree_files` call.
    pub fail_next_worktree_list: bool,
    /// What `refs` answers.
    pub listing: RefListing,
    /// What `head` answers.
    pub head: Head,
    /// What `default_branch` answers.
    pub default_branch: Option<DefaultBranch>,
    /// What `tree_of` answers per commit.
    pub tree_of: HashMap<Oid, Oid>,
    /// Per oid, how long `tree_of`, `tree_entries`, `read_blob`, `diff_trees` and a log page
    /// take.
    pub delays: HashMap<Oid, Duration>,
    /// Per oid, holds the next `tree_of`, `tree_entries` or `read_blob` call for it, then
    /// clears itself.
    pub gates: HashMap<Oid, Arc<CallGate>>,
    /// `tree_entries` calls.
    pub tree_entry_calls: usize,
    /// Fails the next `tree_entries` call.
    pub fail_next_tree: bool,
    /// Fails the next `refs` call.
    pub fail_next_refs: bool,
    /// How long each `refs` call takes.
    pub refs_delay: Duration,
    /// Holds the next `refs` call, then clears itself.
    pub refs_gate: Option<Arc<CallGate>>,
    /// `refs` calls.
    pub refs_calls: usize,
    /// Per blob, what a content search finds in it; any other blob holds no match.
    pub blob_searches: HashMap<Oid, FileSearch>,
    /// Per working-tree path, what a content search finds in it.
    pub worktree_searches: HashMap<String, FileSearch>,
    /// The oids each `search_blobs` call asked for, in call order.
    pub search_blob_calls: Vec<Vec<Oid>>,
    /// The paths each `search_worktree_files` call asked for, in call order.
    pub search_worktree_calls: Vec<Vec<String>>,
    /// When set, a content-search call answers at most this many files, as the core does
    /// once it reaches its byte budget.
    pub content_files_per_call: Option<usize>,
    /// Holds the next content-search call, then clears itself.
    pub content_search_gate: Option<Arc<CallGate>>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            trees: HashMap::new(),
            blobs: HashMap::new(),
            blob_sizes: HashMap::new(),
            read_blob_calls: 0,
            commits: HashMap::new(),
            summaries: HashMap::new(),
            details: HashMap::new(),
            path_histories: HashMap::new(),
            path_history_calls: 0,
            path_history_fail_from: None,
            path_matches_per_call: None,
            path_history_delay: Duration::ZERO,
            resolves: HashMap::new(),
            resolve_gates: HashMap::new(),
            commit_gates: HashMap::new(),
            search_calls: 0,
            search_max_scans: Vec::new(),
            search_gate: None,
            log_calls: 0,
            tree_diffs: HashMap::new(),
            diff_tree_calls: Vec::new(),
            objects: HashSet::new(),
            merge_bases: HashMap::new(),
            merge_base_calls: Vec::new(),
            blob_diffs: HashMap::new(),
            diff_blobs_calls: 0,
            blames: HashMap::new(),
            blame_limits: Vec::new(),
            blame_tips: Vec::new(),
            blame_floors: Vec::new(),
            blame_gate: None,
            worktree_listings: HashMap::new(),
            worktree_contents: HashMap::new(),
            worktree_content_calls: 0,
            status_report: StatusReport::default(),
            status_calls: 0,
            status_trees: Vec::new(),
            fail_next_status: false,
            status_gate: None,
            worktree_diffs: HashMap::new(),
            worktree_diff_calls: HashMap::new(),
            worktree_diff_gates: HashMap::new(),
            stashes: Vec::new(),
            stash_calls: 0,
            stash_gate: None,
            fail_next_stash: false,
            work_dir: None,
            worktree_file_list: WorktreeFileList::default(),
            worktree_list_calls: 0,
            worktree_list_delay: Duration::ZERO,
            fail_next_worktree_list: false,
            listing: RefListing::default(),
            head: Head {
                ref_name: None,
                target: None,
                exact_ref_name: true,
            },
            default_branch: None,
            tree_of: HashMap::new(),
            delays: HashMap::new(),
            gates: HashMap::new(),
            tree_entry_calls: 0,
            fail_next_tree: false,
            fail_next_refs: false,
            refs_delay: Duration::ZERO,
            refs_gate: None,
            refs_calls: 0,
            blob_searches: HashMap::new(),
            worktree_searches: HashMap::new(),
            search_blob_calls: Vec::new(),
            search_worktree_calls: Vec::new(),
            content_files_per_call: None,
            content_search_gate: None,
        }
    }
}

/// An in-memory repository. A call can be held at a [`CallGate`] so a test can race two loads
/// and check which result wins; each gate holds only the first call it meets. Cloning shares
/// the state.
#[derive(Debug, Clone, Default)]
pub struct FakeRepository {
    state: Arc<Mutex<FakeState>>,
}

/// Sleeps outside the lock, when there is anything to sleep.
fn pause(delay: Duration) {
    if !delay.is_zero() {
        thread::sleep(delay);
    }
}

/// Passes `gate`, when there is one.
fn pass(gate: Option<Arc<CallGate>>) {
    if let Some(gate) = gate {
        gate.pass();
    }
}

fn fail<T>(message: impl Into<String>) -> ReadResult<T> {
    Err(RepositoryError::new(message))
}

impl FakeRepository {
    /// An empty, bare fake.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The answers and counters, locked.
    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One page of a log cursor that has handed out `skip` commits; counts in `log_calls`.
    fn log_page(&self, tip: Oid, skip: usize, limit: usize) -> ReadResult<Vec<CommitSummary>> {
        let (list, delay) = {
            let mut state = self.state();
            state.log_calls += 1;
            (
                state.commits.get(&tip).cloned(),
                state.delays.get(&tip).copied().unwrap_or_default(),
            )
        };
        pause(delay);
        let Some(list) = list else {
            return fail(format!("read object header: object {tip}: missing"));
        };
        Ok(list.into_iter().skip(skip).take(limit).collect())
    }

    /// One page of a path walk standing at `scan_skip`; counts in `path_history_calls`.
    fn path_page(
        &self,
        tip: Oid,
        path: &str,
        scan_skip: usize,
        take: Take,
    ) -> ReadResult<CommitPage> {
        let (found, fail_from, cap, delay) = {
            let mut state = self.state();
            state.path_history_calls += 1;
            (
                state.path_histories.get(&(tip, path.to_owned())).cloned(),
                state.path_history_fail_from,
                state.path_matches_per_call,
                state.path_history_delay,
            )
        };
        pause(delay);
        if fail_from.is_some_and(|from| scan_skip >= from) {
            return fail(format!("read commit: object {tip}: missing"));
        }
        let Some(found) = found else {
            return fail(format!("read object header: object {tip}: missing"));
        };
        let count = take.limit.min(take.max_scan).min(cap.unwrap_or(usize::MAX));
        let slice: Vec<CommitSummary> = found.iter().skip(scan_skip).take(count).cloned().collect();
        let scanned = slice.len();
        Ok(CommitPage {
            commits: slice,
            scanned,
            complete: scan_skip + scanned >= found.len(),
        })
    }

    /// Filters the commits stored for `tip` by title, author and membership in the stored
    /// path history, one scanned per stored commit, so `scan_skip` indexes the unfiltered
    /// list.
    fn search_page(
        &self,
        tip: Oid,
        filter: &CommitFilter,
        scan_skip: usize,
        take: Take,
    ) -> ReadResult<CommitPage> {
        let (found, delay, touching, gate) = {
            let mut state = self.state();
            state.search_calls += 1;
            state.search_max_scans.push(take.max_scan);
            let touching = filter.path.as_ref().map(|path| {
                let key = (tip, path.clone());
                let list = state.path_histories.get(&key).cloned().unwrap_or_default();
                list.into_iter()
                    .map(|commit| commit.oid)
                    .collect::<Vec<_>>()
            });
            (
                state.commits.get(&tip).cloned(),
                state.delays.get(&tip).copied().unwrap_or_default(),
                touching,
                state.search_gate.take(),
            )
        };
        pass(gate);
        pause(delay);
        let Some(found) = found else {
            return fail(format!("read object header: object {tip}: missing"));
        };
        let mut matched = Vec::new();
        let mut scanned = 0;
        for commit in found.iter().skip(scan_skip).take(take.max_scan) {
            scanned += 1;
            if matches_filter(commit, filter, touching.as_deref()) {
                matched.push(commit.clone());
                if matched.len() >= take.limit {
                    break;
                }
            }
        }
        Ok(CommitPage {
            commits: matched,
            scanned,
            complete: scan_skip + scanned >= found.len(),
        })
    }

    fn content_search<K: Clone, F>(&self, keys: &[K], record: F) -> SearchBatch
    where
        F: FnOnce(&mut FakeState, &[K]) -> SearchBatch,
    {
        let (results, gate) = {
            let mut state = self.state();
            let answered = state
                .content_files_per_call
                .unwrap_or(keys.len())
                .min(keys.len());
            let results = record(&mut state, &keys[..answered]);
            (results, state.content_search_gate.take())
        };
        pass(gate);
        results
    }
}

/// How much one walk page may take.
#[derive(Debug, Clone, Copy)]
struct Take {
    limit: usize,
    max_scan: usize,
}

fn contains_folded(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn matches_filter(commit: &CommitSummary, filter: &CommitFilter, touching: Option<&[Oid]>) -> bool {
    let message = filter
        .message
        .as_ref()
        .is_none_or(|text| contains_folded(&commit.title, text));
    let author = filter.author.as_ref().is_none_or(|text| {
        contains_folded(&commit.author_name, text) || contains_folded(&commit.author_email, text)
    });
    let path = touching.is_none_or(|oids| oids.contains(&commit.oid));
    message && author && path
}

fn empty_search() -> FileSearch {
    FileSearch::Searched {
        lines: Vec::new(),
        truncated: false,
    }
}

impl RepositoryReading for FakeRepository {
    fn work_dir(&self) -> Option<PathBuf> {
        self.state().work_dir.clone()
    }

    fn head(&self) -> ReadResult<Head> {
        Ok(self.state().head.clone())
    }

    fn default_branch(&self) -> ReadResult<Option<DefaultBranch>> {
        Ok(self.state().default_branch.clone())
    }

    fn refs(&self) -> ReadResult<RefListing> {
        let (listing, failing, delay, gate) = {
            let mut state = self.state();
            state.refs_calls += 1;
            let failing = std::mem::take(&mut state.fail_next_refs);
            (
                state.listing.clone(),
                failing,
                state.refs_delay,
                state.refs_gate.take(),
            )
        };
        pass(gate);
        pause(delay);
        if failing {
            return fail("cannot list refs: transient lock");
        }
        Ok(listing)
    }

    fn resolve(&self, spec: &str) -> ReadResult<Oid> {
        let (found, gate) = {
            let mut state = self.state();
            (
                state.resolves.get(spec).copied(),
                state.resolve_gates.remove(spec),
            )
        };
        pass(gate);
        found.ok_or_else(|| {
            RepositoryError::new(format!("'{spec}' is not a revision in this repository"))
        })
    }

    fn resolve_tree(&self, spec: &str) -> ReadResult<Oid> {
        let state = self.state();
        let mapped = state
            .resolves
            .get(spec)
            .and_then(|oid| state.tree_of.get(oid));
        mapped.copied().ok_or_else(|| {
            RepositoryError::new(format!("'{spec}' is not a revision in this repository"))
        })
    }

    fn tree_of(&self, oid: Oid) -> ReadResult<Oid> {
        let (mapped, delay, gate) = {
            let mut state = self.state();
            (
                state.tree_of.get(&oid).copied(),
                state.delays.get(&oid).copied().unwrap_or_default(),
                state.gates.remove(&oid),
            )
        };
        pass(gate);
        pause(delay);
        mapped
            .ok_or_else(|| RepositoryError::new(format!("object {oid} is a blob, expected a tree")))
    }

    fn tree_entries(&self, tree: Oid) -> ReadResult<Vec<TreeEntry>> {
        let (entries, failing, delay, gate) = {
            let mut state = self.state();
            state.tree_entry_calls += 1;
            let failing = std::mem::take(&mut state.fail_next_tree);
            (
                state.trees.get(&tree).cloned(),
                failing,
                state.delays.get(&tree).copied().unwrap_or_default(),
                state.gates.remove(&tree),
            )
        };
        pass(gate);
        pause(delay);
        match entries {
            Some(entries) if !failing => Ok(entries),
            Some(_) | None => fail(format!("read tree: object {tree} is missing or unreadable")),
        }
    }

    fn read_blob(&self, oid: Oid, limit: u64) -> ReadResult<BlobContent> {
        let (content, claimed, delay, gate) = {
            let mut state = self.state();
            state.read_blob_calls += 1;
            (
                state.blobs.get(&oid).cloned(),
                state.blob_sizes.get(&oid).copied(),
                state.delays.get(&oid).copied().unwrap_or_default(),
                state.gates.remove(&oid),
            )
        };
        pass(gate);
        pause(delay);
        let Some(content) = content else {
            return fail(format!("read blob header: object {oid}: missing"));
        };
        if let Some(size) = claimed.filter(|size| *size > limit) {
            return Ok(BlobContent::TooLarge { size, limit });
        }
        Ok(over_limit(content, limit))
    }

    fn log_cursor(&self, tip: Oid) -> ReadResult<Box<dyn LogCursorReading>> {
        Ok(Box::new(FakeLogCursor {
            repository: self.clone(),
            tip,
            position: Mutex::new(0),
        }))
    }

    fn commit(&self, oid: Oid) -> ReadResult<CommitSummary> {
        let (found, gate) = {
            let mut state = self.state();
            let found = state.summaries.get(&oid).cloned().or_else(|| {
                state
                    .commits
                    .values()
                    .flatten()
                    .find(|commit| commit.oid == oid)
                    .cloned()
            });
            (found, state.commit_gates.remove(&oid))
        };
        pass(gate);
        found.ok_or_else(|| RepositoryError::new(format!("read commit: object {oid}: missing")))
    }

    fn commit_details(&self, oid: Oid) -> ReadResult<CommitDetail> {
        if let Some(found) = self.state().details.get(&oid).cloned() {
            return Ok(found);
        }
        let summary = self
            .commit(oid)
            .unwrap_or_else(|_| commit_summary(oid, &[], "change"));
        Ok(CommitDetail {
            body: String::new(),
            committer_name: summary.author_name.clone(),
            committer_email: summary.author_email.clone(),
            committer_time: summary.author_time,
            committer_offset: summary.author_offset,
            tree: oid,
            summary,
        })
    }

    fn history_walk(
        &self,
        tip: Oid,
        filter: CommitFilter,
    ) -> ReadResult<Box<dyn HistoryWalkReading>> {
        Ok(Box::new(FakeHistoryWalk {
            repository: self.clone(),
            tip,
            filter,
            position: Mutex::new(0),
        }))
    }

    fn diff_trees(&self, from: Option<Oid>, to: Oid) -> ReadResult<Vec<TreeChange>> {
        let (found, delay) = {
            let mut state = self.state();
            state.diff_tree_calls.push((from, to));
            (
                state.tree_diffs.get(&to).cloned(),
                state.delays.get(&to).copied().unwrap_or_default(),
            )
        };
        pause(delay);
        found.ok_or_else(|| {
            RepositoryError::new(format!("read tree: object {to} is missing or unreadable"))
        })
    }

    fn has_object(&self, oid: Oid) -> bool {
        self.state().objects.contains(&oid)
    }

    fn merge_base(&self, one: Oid, two: Oid, max_scan: usize) -> ReadResult<MergeBase> {
        let found = {
            let mut state = self.state();
            state.merge_base_calls.push((one, two, max_scan));
            state.merge_bases.get(&(one, two)).copied()
        };
        found.ok_or_else(|| RepositoryError::new(format!("read commit: object {two}: missing")))
    }

    fn diff_blobs(
        &self,
        old: Option<Oid>,
        new: Option<Oid>,
        _limit: u64,
        _context: u32,
    ) -> ReadResult<BlobDiff> {
        let found = {
            let mut state = self.state();
            state.diff_blobs_calls += 1;
            state.blob_diffs.get(&(old, new)).cloned()
        };
        found.ok_or_else(|| {
            let side = old
                .or(new)
                .map_or_else(|| "none".to_owned(), |oid| oid.to_string());
            RepositoryError::new(format!("read blob header: object {side}: missing"))
        })
    }

    fn blame(&self, tip: Oid, path: &str, limit: u64, since: Option<i64>) -> ReadResult<Blame> {
        let (found, gate) = {
            let mut state = self.state();
            state.blame_limits.push(limit);
            state.blame_tips.push(tip);
            state.blame_floors.push(since);
            (state.blames.get(path).cloned(), state.blame_gate.take())
        };
        pass(gate);
        found.ok_or_else(|| {
            RepositoryError::new(format!("blame file: '{path}' does not exist at {tip}"))
        })
    }

    fn worktree_entries(&self, _tree: Option<Oid>, dir: &str) -> ReadResult<WorktreeListing> {
        let found = self.state().worktree_listings.get(dir).cloned();
        found.ok_or_else(|| {
            RepositoryError::new(format!("list working tree: '{dir}': cannot be read"))
        })
    }

    fn worktree_file(&self, path: &str, limit: u64) -> ReadResult<BlobContent> {
        let found = {
            let mut state = self.state();
            state.worktree_content_calls += 1;
            state.worktree_contents.get(path).cloned()
        };
        let Some(found) = found else {
            return fail(format!("read working tree file: '{path}': missing"));
        };
        Ok(over_limit(found, limit))
    }

    fn worktree_status(&self, tree: Option<Oid>, _max_scan: usize) -> ReadResult<StatusReport> {
        let (report, gate, failing) = {
            let mut state = self.state();
            state.status_calls += 1;
            state.status_trees.push(tree);
            let failing = std::mem::take(&mut state.fail_next_status);
            (
                state.status_report.clone(),
                state.status_gate.take(),
                failing,
            )
        };
        pass(gate);
        if failing {
            return fail("scan working tree: transient failure");
        }
        Ok(report)
    }

    fn worktree_files(&self, _tree: Option<Oid>, _max_scan: usize) -> ReadResult<WorktreeFileList> {
        let (list, failing, delay) = {
            let mut state = self.state();
            state.worktree_list_calls += 1;
            let failing = std::mem::take(&mut state.fail_next_worktree_list);
            (
                state.worktree_file_list.clone(),
                failing,
                state.worktree_list_delay,
            )
        };
        pause(delay);
        if failing {
            return fail("walk working tree: transient failure");
        }
        Ok(list)
    }

    fn diff_worktree_file(
        &self,
        path: &str,
        _old: Option<Oid>,
        _limit: u64,
        _context: u32,
    ) -> ReadResult<BlobDiff> {
        let (found, gate) = {
            let mut state = self.state();
            *state
                .worktree_diff_calls
                .entry(path.to_owned())
                .or_default() += 1;
            (
                state.worktree_diffs.get(path).cloned(),
                state.worktree_diff_gates.remove(path),
            )
        };
        pass(gate);
        found.ok_or_else(|| {
            RepositoryError::new(format!("read working tree file: '{path}': missing"))
        })
    }

    fn stash_list(&self) -> ReadResult<Vec<StashEntry>> {
        let (entries, failing, gate) = {
            let mut state = self.state();
            state.stash_calls += 1;
            let failing = std::mem::take(&mut state.fail_next_stash);
            (state.stashes.clone(), failing, state.stash_gate.take())
        };
        pass(gate);
        if failing {
            return fail("list stashes: transient failure");
        }
        Ok(entries)
    }

    fn search_blobs(
        &self,
        oids: &[Oid],
        _query: &ContentQuery,
        _limits: SearchLimits,
    ) -> ReadResult<SearchBatch> {
        Ok(self.content_search(oids, |state, answered| {
            state.search_blob_calls.push(oids.to_vec());
            let mut results = Vec::with_capacity(answered.len());
            for oid in answered {
                results.push(
                    state
                        .blob_searches
                        .get(oid)
                        .cloned()
                        .unwrap_or_else(empty_search),
                );
            }
            results
        }))
    }

    fn search_worktree_files(
        &self,
        paths: &[String],
        _query: &ContentQuery,
        _limits: SearchLimits,
    ) -> ReadResult<SearchBatch> {
        Ok(self.content_search(paths, |state, answered| {
            state.search_worktree_calls.push(paths.to_vec());
            let mut results = Vec::with_capacity(answered.len());
            for path in answered {
                let found = state.worktree_searches.get(path).cloned();
                results.push(found.unwrap_or_else(empty_search));
            }
            results
        }))
    }
}

/// `content`, or its too-large answer when its text runs past `limit`.
fn over_limit(content: BlobContent, limit: u64) -> BlobContent {
    match content {
        BlobContent::Text { ref data, .. } if data.len() as u64 > limit => BlobContent::TooLarge {
            size: data.len() as u64,
            limit,
        },
        BlobContent::Text { .. } | BlobContent::Binary { .. } | BlobContent::TooLarge { .. } => {
            content
        }
    }
}

/// A log cursor over [`FakeState::commits`]: it remembers how many commits it handed out.
struct FakeLogCursor {
    repository: FakeRepository,
    tip: Oid,
    position: Mutex<usize>,
}

impl LogCursorReading for FakeLogCursor {
    fn next(&self, limit: usize) -> ReadResult<Vec<CommitSummary>> {
        let mut position = self.position.lock().unwrap_or_else(PoisonError::into_inner);
        let page = self.repository.log_page(self.tip, *position, limit)?;
        *position += page.len();
        Ok(page)
    }
}

/// A filtered walk over the fake's stored lists: it remembers how many commits it examined. A
/// filter that names only a path reads [`FakeState::path_histories`]; any other reads
/// [`FakeState::commits`].
struct FakeHistoryWalk {
    repository: FakeRepository,
    tip: Oid,
    filter: CommitFilter,
    position: Mutex<usize>,
}

impl HistoryWalkReading for FakeHistoryWalk {
    fn next(&self, limit: usize, max_scan: usize) -> ReadResult<CommitPage> {
        let mut position = self.position.lock().unwrap_or_else(PoisonError::into_inner);
        let take = Take { limit, max_scan };
        let path_only = CommitFilter {
            path: self.filter.path.clone(),
            ..CommitFilter::default()
        };
        let page = match &self.filter.path {
            Some(path) if self.filter == path_only => {
                self.repository.path_page(self.tip, path, *position, take)?
            }
            Some(_) | None => {
                self.repository
                    .search_page(self.tip, &self.filter, *position, take)?
            }
        };
        *position += page.scanned;
        Ok(page)
    }
}

/// A commit by "Test Author" at 2024-01-01, as the Swift tests build them.
#[must_use]
pub fn commit_summary(oid: Oid, parents: &[Oid], title: &str) -> CommitSummary {
    CommitSummary {
        oid,
        parents: parents.to_vec(),
        author_name: "Test Author".to_owned(),
        author_email: "author@example.com".to_owned(),
        author_time: 1_704_067_200,
        author_offset: 0,
        committer_time: 1_704_067_200,
        title: title.to_owned(),
    }
}

/// A text blob holding `text`, with the line starts core would report.
#[must_use]
pub fn text_blob(text: &str) -> BlobContent {
    let data = text.as_bytes().to_vec();
    let mut line_starts = if data.is_empty() { Vec::new() } else { vec![0] };
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' && index + 1 < data.len() {
            line_starts.push(index + 1);
        }
    }
    BlobContent::Text {
        data,
        line_starts,
        valid_utf8: true,
    }
}
