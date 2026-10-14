//! Filtered history walks: which commits touched a path, or match a search.

use std::collections::HashMap;
use std::sync::Mutex;

use gix::ObjectId;
use gix::objs::tree::{EntryKind as GixEntryKind, EntryMode};
use gix::traverse::commit::Info;

use crate::error::{Result, git, object};
use crate::log::{CommitSummary, summarize};
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::lookup_path;
use crate::walk::{self, Traversal};

/// One page of a filtered history walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPage {
    /// The commits that matched, newest commit time first.
    pub commits: Vec<CommitSummary>,
    /// How many commits this page examined, matched or not.
    pub scanned: usize,
    /// True when the walk reached the end of history within this page.
    pub complete: bool,
}

/// What a [`HistoryWalk`] matches. Every set field must match; an empty filter matches every
/// commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitFilter {
    /// Case-insensitive (ASCII) substring of the full message, title and body.
    pub message: Option<String>,
    /// Case-insensitive (ASCII) substring of the author name or email.
    pub author: Option<String>,
    /// Only commits whose committer time is at or after this time, seconds since the Unix
    /// epoch. Also terminates the walk: nothing older is examined. Committer time can differ
    /// from the author time the CLI and UI show.
    pub after: Option<i64>,
    /// Only commits whose committer time is at or before this time, seconds since the Unix
    /// epoch. Committer time can differ from the author time the CLI and UI show.
    pub before: Option<i64>,
    /// Only commits whose entry at this path differs from the first parent's — from every
    /// parent's for merges, so a clean merge stays hidden. A root commit matches when the
    /// entry exists, and so does a shallow boundary commit, whose parents are absent. Empty
    /// and `.` segments are ignored, so `./a` names `a`; an empty path names the root tree,
    /// so every commit that changed anything matches. Renames are not followed. Checked last,
    /// since it reads two tree entries per commit.
    pub path: Option<String>,
}

/// A filtered walk over the commits reachable from one tip, newest commit time first, taken
/// a page at a time. Each [`next_page`](Self::next_page) resumes where the previous one
/// stopped, so a sparse filter over a long history costs one pass in total however many pages
/// it takes. The walk owns its state rather than a pooled repository handle, and may be used
/// from any thread; pages are taken one at a time. Pass the walk the [`Repository`] that
/// made it.
pub struct HistoryWalk {
    state: Mutex<FilteredWalk>,
}

impl std::fmt::Debug for HistoryWalk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryWalk").finish_non_exhaustive()
    }
}

/// What one [`HistoryWalk`] carries between pages.
struct FilteredWalk {
    traversal: Traversal,
    filter: CommitFilter,
    path_test: Option<PathTest>,
}

impl Repository {
    /// A walk over the commits reachable from `tip` that match every set field of `filter`,
    /// positioned before the newest. A time floor (`after`) ends the walk instead of
    /// filtering, so dated searches never scan past their window. An annotated tag `tip` is
    /// followed to its commit.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when `tip` is, or tags, a tree or blob,
    /// [`Error::Object`](crate::Error::Object) when it is missing,
    /// [`Error::Git`](crate::Error::Git) when the walk cannot start or the shallow file is
    /// unreadable.
    pub fn history_walk(&self, tip: Oid, filter: CommitFilter) -> Result<HistoryWalk> {
        self.with(|repo| {
            let traversal = Traversal::start(repo, tip, filter.after)?;
            let path_test = match &filter.path {
                Some(path) => Some(PathTest::new(repo, path)?),
                None => None,
            };
            Ok(HistoryWalk {
                state: Mutex::new(FilteredWalk {
                    traversal,
                    filter,
                    path_test,
                }),
            })
        })
    }
}

impl HistoryWalk {
    /// The next page: at most `limit` matches, from at most `max_scan` commits examined,
    /// whatever they match. The cap keeps one call from holding a repository handle while it
    /// scans a long history; loop until [`CommitPage::complete`]. A `max_scan` of 0 counts as
    /// 1, so every incomplete page advances. Message and author matching decode each examined
    /// commit, a time ceiling (`before`) is checked before any decode, and a path is checked
    /// last.
    ///
    /// # Errors
    /// [`Error::Git`](crate::Error::Git) when the walk cannot advance or an earlier page of
    /// this walk failed, [`Error::Object`](crate::Error::Object) when a commit, or with a
    /// `path` a tree, on the way is unreadable.
    pub fn next_page(
        &self,
        repo: &Repository,
        limit: usize,
        max_scan: usize,
    ) -> Result<CommitPage> {
        let max_scan = max_scan.max(1);
        let mut guard = walk::lock(&self.state)?;
        let state = &mut *guard;
        state.traversal.ensure_usable()?;
        let result = repo.with(|repo| state.page(repo, limit, max_scan));
        if result.is_err() {
            state.traversal.mark_broken();
        }
        result
    }
}

impl FilteredWalk {
    fn page(
        &mut self,
        repo: &gix::Repository,
        limit: usize,
        max_scan: usize,
    ) -> Result<CommitPage> {
        if let Some(test) = self.path_test.as_mut() {
            // The memo serves one page; a fresh one costs a single extra tree read.
            test.lookup = Lookup::open(repo);
        }
        let mut commits = Vec::new();
        let mut scanned = 0;
        while let Some(info) = self.traversal.next_commit()? {
            if commits.len() >= limit || scanned >= max_scan {
                self.traversal.put_back(info);
                return Ok(CommitPage {
                    commits,
                    scanned,
                    complete: false,
                });
            }
            scanned += 1;
            if let Some(summary) = self.examine(repo, &info)? {
                commits.push(summary);
            }
        }
        Ok(CommitPage {
            commits,
            scanned,
            complete: true,
        })
    }

    /// The summary of the commit walked as `info` when it matches the filter.
    fn examine(&mut self, repo: &gix::Repository, info: &Info) -> Result<Option<CommitSummary>> {
        // Time-ordered walks always carry the commit time, so the ceiling costs no decode.
        if let (Some(before), Some(time)) = (self.filter.before, info.commit_time)
            && time > before
        {
            return Ok(None);
        }
        if self.filter.message.is_some() || self.filter.author.is_some() {
            return self.examine_decoded(repo, info);
        }
        // A path alone needs only tree ids, so a commit is read once it is known to match.
        if let Some(test) = self.path_test.as_mut()
            && !test.touches(repo, info)?
        {
            return Ok(None);
        }
        summarize(&read_commit(repo, info.id)?).map(Some)
    }

    /// [`examine`](Self::examine) for a filter that needs the decoded commit anyway.
    fn examine_decoded(
        &mut self,
        repo: &gix::Repository,
        info: &Info,
    ) -> Result<Option<CommitSummary>> {
        let oid = Oid::from_gix(info.id);
        let commit = read_commit(repo, info.id)?;
        let summary = summarize(&commit)?;
        if !matches(&self.filter, &commit, &summary).map_err(object("decode commit", oid))? {
            return Ok(None);
        }
        if let Some(test) = self.path_test.as_mut() {
            test.lookup.remember(info.id, &commit)?;
            if !test.touches(repo, info)? {
                return Ok(None);
            }
        }
        Ok(Some(summary))
    }
}

fn read_commit(repo: &gix::Repository, id: ObjectId) -> Result<gix::Commit<'_>> {
    repo.find_commit(id)
        .map_err(object("read commit", Oid::from_gix(id)))
}

/// Whether one decoded commit matches the message and author fields of `filter`. Time fields
/// are handled by the walk.
fn matches(
    filter: &CommitFilter,
    commit: &gix::Commit<'_>,
    summary: &CommitSummary,
) -> std::result::Result<bool, gix::objs::decode::Error> {
    if let Some(needle) = &filter.message
        && !contains_ignore_ascii_case(commit.decode()?.message, needle.as_bytes())
    {
        return Ok(false);
    }
    if let Some(needle) = &filter.author {
        let needle = needle.as_bytes();
        if !contains_ignore_ascii_case(summary.author_name.as_bytes(), needle)
            && !contains_ignore_ascii_case(summary.author_email.as_bytes(), needle)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// The per-commit path-change test of one walk: which commits changed the entry at a path.
struct PathTest {
    /// The path split on `/`, empty and `.` segments dropped.
    segments: Vec<Vec<u8>>,
    /// The shallow boundary commits, sorted, or `None` in a complete clone.
    shallow: Option<gix::shallow::Commits>,
    lookup: Lookup,
}

impl PathTest {
    fn new(repo: &gix::Repository, path: &str) -> Result<Self> {
        let segments = path
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .map(|segment| segment.as_bytes().to_vec())
            .collect();
        let shallow = repo.shallow_commits().map_err(git("read shallow file"))?;
        Ok(Self {
            segments,
            shallow,
            lookup: Lookup::default(),
        })
    }

    /// Whether the commit walked as `info` changed the entry at the path.
    fn touches(&mut self, repo: &gix::Repository, info: &Info) -> Result<bool> {
        let tree = self.lookup.tree_of(repo, info.id)?;
        // A shallow boundary lists parents the clone does not have; git judges it as a root.
        // A parent missing anywhere else is corruption and still errors.
        let is_boundary = self
            .shallow
            .as_ref()
            .is_some_and(|commits| commits.binary_search(&info.id).is_ok());
        let parents: &[ObjectId] = if is_boundary { &[] } else { &info.parent_ids };
        self.lookup.touches(repo, tree, parents, &self.segments)
    }
}

/// Memoized tree-entry lookups for one walk. Both maps die with the call; nothing here is a
/// persistent index.
#[derive(Default)]
struct Lookup {
    /// The repository's commit-graph, which stores each commit's root tree, so most commits
    /// are never read from the object database. Only a cache: every miss reads the commit.
    graph: Option<gix::commitgraph::Graph>,
    /// Root tree per commit, so a parent decoded while judging its child is not decoded again.
    trees: HashMap<ObjectId, ObjectId>,
    /// Entry at the searched path per root tree. Commit N's parent tree is commit N+1's own
    /// tree, so each tree is searched once.
    entries: HashMap<ObjectId, Option<(ObjectId, EntryMode)>>,
}

impl Lookup {
    /// Whether the entry at `segments` in `tree` differs from every parent's.
    fn touches(
        &mut self,
        repo: &gix::Repository,
        tree: ObjectId,
        parents: &[ObjectId],
        segments: &[Vec<u8>],
    ) -> Result<bool> {
        let own = self.entry(repo, tree, segments)?;
        if parents.is_empty() {
            return Ok(own.is_some());
        }
        for parent in parents {
            let parent_tree = self.tree_of(repo, *parent)?;
            // Same root tree means the commit is empty relative to this parent.
            if parent_tree == tree || self.entry(repo, parent_tree, segments)? == own {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// An empty memo over the repository's commit-graph. An unreadable or disabled graph only
    /// costs speed, as it does in the walk itself: every commit is then read instead.
    fn open(repo: &gix::Repository) -> Self {
        Self {
            graph: repo.commit_graph_if_enabled().ok().flatten(),
            ..Self::default()
        }
    }

    /// The root tree of `commit`: from the memo, else the commit-graph, else the commit
    /// itself, which covers commits newer than the last graph write.
    fn tree_of(&mut self, repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
        if let Some(tree) = self.trees.get(&commit) {
            return Ok(*tree);
        }
        let cached = self.graph.as_ref().and_then(|graph| {
            graph
                .commit_by_id(commit)
                .map(|entry| entry.root_tree_id().to_owned())
        });
        let tree = match cached {
            Some(tree) => tree,
            None => commit_tree(&read_commit(repo, commit)?, commit)?,
        };
        self.trees.insert(commit, tree);
        Ok(tree)
    }

    /// Memoizes the root tree of `commit`, already read as `id`.
    fn remember(&mut self, id: ObjectId, commit: &gix::Commit<'_>) -> Result<()> {
        let tree = commit_tree(commit, id)?;
        self.trees.insert(id, tree);
        Ok(())
    }

    fn entry(
        &mut self,
        repo: &gix::Repository,
        tree: ObjectId,
        segments: &[Vec<u8>],
    ) -> Result<Option<(ObjectId, EntryMode)>> {
        if segments.is_empty() {
            return Ok(Some((tree, GixEntryKind::Tree.into())));
        }
        if let Some(found) = self.entries.get(&tree) {
            return Ok(*found);
        }
        let found = lookup_path(
            repo,
            Oid::from_gix(tree),
            segments.iter().map(Vec::as_slice),
        )?
        .map(|entry| (entry.oid, entry.mode));
        self.entries.insert(tree, found);
        Ok(found)
    }
}

fn commit_tree(commit: &gix::Commit<'_>, id: ObjectId) -> Result<ObjectId> {
    Ok(commit
        .tree_id()
        .map_err(object("decode commit", Oid::from_gix(id)))?
        .detach())
}
