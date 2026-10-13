//! Filtered history walks: which commits touched a path, or match a search.

use std::collections::HashMap;

use gix::ObjectId;
use gix::objs::tree::{EntryKind as GixEntryKind, EntryMode};
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::error::{Result, git, object};
use crate::log::{CommitSummary, peel_to_commit, summarize};
use crate::oid::Oid;
use crate::repo::Repository;
use crate::tree::lookup_path;

/// One page of a filtered history walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPage {
    /// The commits that matched, newest commit time first.
    pub commits: Vec<CommitSummary>,
    /// How many commits this call examined past `scan_skip`. The next page passes
    /// `scan_skip + scanned` back, so the walk resumes where this call stopped.
    pub scanned: usize,
    /// True when the walk reached the end of history within this call.
    pub complete: bool,
}

/// What `search_commits` matches. Every set field must match; an empty filter matches every
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
    /// Only commits that changed the entry at this path, judged as
    /// [`path_history`](Repository::path_history) judges it. Checked last, since it reads two
    /// tree entries per commit.
    pub path: Option<String>,
}

impl Repository {
    /// Commits reachable from `tip` whose entry at `path` differs from the first parent's —
    /// from every parent's for merges, so a clean merge stays hidden — newest commit time
    /// first. A root commit matches when the entry exists. Empty and `.` segments are
    /// ignored, so `./a` names `a`. An empty `path` names the root tree, so every commit that
    /// changed anything matches. Renames are not followed; blame covers deeper archaeology.
    /// In a shallow clone a boundary commit is judged like a root, since its parents are absent.
    ///
    /// Like `log`, the walk restarts from `tip` and skips `scan_skip` commits, so keep pages
    /// contiguous and cache them. At most `max_scan` commits are examined per call, whatever
    /// they match: this walk reads two tree entries per commit, and the cap keeps one call
    /// from holding a repository handle while it scans a long history. Loop on
    /// [`CommitPage::scanned`] until [`CommitPage::complete`]. A `max_scan` of 0 counts as
    /// 1, so every incomplete page advances the cursor.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when `tip` is, or tags, a tree or blob; an
    /// annotated tag is followed to its commit.
    /// [`Error::Git`](crate::Error::Git) when the walk cannot start or advance or the shallow
    /// file is unreadable, [`Error::Object`](crate::Error::Object) when a commit or tree on the
    /// way is unreadable.
    pub fn path_history(
        &self,
        tip: Oid,
        path: &str,
        scan_skip: usize,
        limit: usize,
        max_scan: usize,
    ) -> Result<CommitPage> {
        let max_scan = max_scan.max(1);
        self.with(|repo| {
            let tip = peel_to_commit(repo, tip)?.id;
            let walk = repo
                .rev_walk([tip])
                .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
                .all()
                .map_err(git("start commit walk"))?;
            let mut test = PathTest::new(repo, path)?;
            let mut commits = Vec::new();
            let mut scanned = 0;
            let mut complete = true;
            for info in walk.skip(scan_skip) {
                if commits.len() >= limit || scanned >= max_scan {
                    complete = false;
                    break;
                }
                scanned += 1;
                let info = info.map_err(git("walk commits"))?;
                let oid = Oid::from_gix(info.id);
                let commit = info.object().map_err(object("read commit", oid))?;
                if test.touches(repo, &info, &commit)? {
                    commits.push(summarize(&commit)?);
                }
            }
            Ok(CommitPage {
                commits,
                scanned,
                complete,
            })
        })
    }

    /// Commits reachable from `tip` that match every set field of `filter`, newest commit
    /// time first. A time floor (`after`) terminates the walk instead of filtering, so dated
    /// searches never scan past their window. A `path` keeps only the commits
    /// [`path_history`](Self::path_history) would list for it.
    ///
    /// Pages like [`path_history`](Self::path_history): at most `max_scan` commits are
    /// examined past `scan_skip`, and the page reports how many, so the caller loops until
    /// [`CommitPage::complete`]; a `max_scan` of 0 counts as 1, so every incomplete page
    /// advances. Message and author matching decode each examined commit —
    /// the same cost class as `log` — and a time ceiling (`before`) is checked before any
    /// decode.
    ///
    /// # Errors
    /// [`Error::WrongKind`](crate::Error::WrongKind) when `tip` is, or tags, a tree or blob; an
    /// annotated tag is followed to its commit.
    /// [`Error::Git`](crate::Error::Git) when the walk cannot start or advance or, with a
    /// `path`, the shallow file is unreadable; [`Error::Object`](crate::Error::Object) when a
    /// commit, or with a `path` a tree, on the way is unreadable.
    pub fn search_commits(
        &self,
        tip: Oid,
        filter: &CommitFilter,
        scan_skip: usize,
        limit: usize,
        max_scan: usize,
    ) -> Result<CommitPage> {
        let max_scan = max_scan.max(1);
        self.with(|repo| {
            let sorting = match filter.after {
                Some(seconds) => Sorting::ByCommitTimeCutoff {
                    order: CommitTimeOrder::NewestFirst,
                    seconds,
                },
                None => Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
            };
            let tip = peel_to_commit(repo, tip)?.id;
            let walk = repo
                .rev_walk([tip])
                .sorting(sorting)
                .all()
                .map_err(git("start commit walk"))?;
            let mut path_test = match &filter.path {
                Some(path) => Some(PathTest::new(repo, path)?),
                None => None,
            };
            let mut commits = Vec::new();
            let mut scanned = 0;
            let mut complete = true;
            for info in walk.skip(scan_skip) {
                if commits.len() >= limit || scanned >= max_scan {
                    complete = false;
                    break;
                }
                scanned += 1;
                let info = info.map_err(git("walk commits"))?;
                let oid = Oid::from_gix(info.id);
                // Time-ordered walks always carry the commit time, so the ceiling costs no
                // decode.
                if let (Some(before), Some(time)) = (filter.before, info.commit_time)
                    && time > before
                {
                    continue;
                }
                let commit = info.object().map_err(object("read commit", oid))?;
                let summary = summarize(&commit)?;
                if !matches(filter, &commit, &summary).map_err(object("decode commit", oid))? {
                    continue;
                }
                if let Some(test) = path_test.as_mut()
                    && !test.touches(repo, &info, &commit)?
                {
                    continue;
                }
                commits.push(summary);
            }
            Ok(CommitPage {
                commits,
                scanned,
                complete,
            })
        })
    }
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

    /// Whether `commit`, walked as `info`, changed the entry at the path.
    fn touches(
        &mut self,
        repo: &gix::Repository,
        info: &gix::revision::walk::Info<'_>,
        commit: &gix::Commit<'_>,
    ) -> Result<bool> {
        let oid = Oid::from_gix(info.id);
        let tree = commit
            .tree_id()
            .map_err(object("decode commit", oid))?
            .detach();
        self.lookup.trees.insert(info.id, tree);
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

    fn tree_of(&mut self, repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
        if let Some(tree) = self.trees.get(&commit) {
            return Ok(*tree);
        }
        let oid = Oid::from_gix(commit);
        let found = repo
            .find_commit(commit)
            .map_err(object("read commit", oid))?;
        let tree = found
            .tree_id()
            .map_err(object("decode commit", oid))?
            .detach();
        self.trees.insert(commit, tree);
        Ok(tree)
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
