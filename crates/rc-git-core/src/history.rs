//! Filtered history walks: which commits touched a path.

use std::collections::HashMap;

use gix::ObjectId;
use gix::objs::tree::{EntryKind as GixEntryKind, EntryMode};
use gix::revision::walk::Sorting;
use gix::traverse::commit::simple::CommitTimeOrder;

use crate::error::{Result, git, object};
use crate::log::summarize;
use crate::oid::Oid;
use crate::repo::Repository;

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

use crate::log::CommitSummary;

impl Repository {
    /// Commits reachable from `tip` whose entry at `path` differs from the first parent's —
    /// from every parent's for merges, so a clean merge stays hidden — newest commit time
    /// first. A root commit matches when the entry exists. An empty `path` names the root
    /// tree, so every commit that changed anything matches. Renames are not followed; blame
    /// covers deeper archaeology.
    ///
    /// Like `log`, the walk restarts from `tip` and skips `scan_skip` commits, so keep pages
    /// contiguous and cache them. At most `max_scan` commits are examined per call, whatever
    /// they match: this walk reads two tree entries per commit, and the cap keeps one call
    /// from holding a repository handle while it scans a long history. Loop on
    /// [`CommitPage::scanned`] until [`CommitPage::complete`].
    ///
    /// # Errors
    /// [`Error::Git`](crate::Error::Git) when the walk cannot start or advance,
    /// [`Error::Object`](crate::Error::Object) when a commit or tree on the way is unreadable.
    pub fn path_history(
        &self,
        tip: Oid,
        path: &str,
        scan_skip: usize,
        limit: usize,
        max_scan: usize,
    ) -> Result<CommitPage> {
        let segments: Vec<Vec<u8>> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.as_bytes().to_vec())
            .collect();
        self.with(|repo| {
            let walk = repo
                .rev_walk([*tip.as_gix()])
                .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
                .all()
                .map_err(git("start commit walk"))?;
            let mut lookup = Lookup::default();
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
                let tree = commit
                    .tree_id()
                    .map_err(object("decode commit", oid))?
                    .detach();
                lookup.trees.insert(info.id, tree);
                if lookup.touches(repo, tree, &info.parent_ids, &segments)? {
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
        let oid = Oid::from_gix(tree);
        let tree_obj = repo.find_tree(tree).map_err(object("read tree", oid))?;
        let found = tree_obj
            .lookup_entry(segments.iter().map(Vec::as_slice))
            .map_err(object("look up path", oid))?
            .map(|entry| (entry.object_id(), entry.mode()));
        self.entries.insert(tree, found);
        Ok(found)
    }
}
