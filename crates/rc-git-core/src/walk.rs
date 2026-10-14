//! The resumable commit-time traversal under `LogCursor` and `HistoryWalk`.
//!
//! A gix `Walk` borrows the pooled `gix::Repository` it came from, so it cannot outlive one
//! call. This traversal owns a clone of the object-database handle instead, which is what lets
//! a page resume where the previous one stopped without holding a pooled repository.

use std::sync::{Mutex, MutexGuard};

use gix::ObjectId;
use gix::traverse::commit::simple::{CommitTimeOrder, Sorting};
use gix::traverse::commit::{Info, Simple};

use crate::error::{Error, Result, git, object};
use crate::log::peel_to_commit;
use crate::oid::Oid;

/// Object cache of a walk's own object-database handle. The walk reads a commit when it
/// queues it and again when it yields it, so the cache only has to span the frontier; every
/// open walk holds one, so it stays far below the pooled handles' cache.
const WALK_OBJECT_CACHE_BYTES: usize = 4 * 1024 * 1024;

/// Decides which parents the traversal queues: in a shallow clone, never the parents a
/// boundary commit lists but the clone does not have.
type ParentFilter = Box<dyn FnMut(&gix::hash::oid) -> bool + Send>;

/// Commits reachable from one tip, newest commit time first, resumable across calls.
pub(crate) struct Traversal {
    simple: Simple<gix::OdbHandle, ParentFilter>,
    /// A commit taken from `simple` but not examined, because the page was already full.
    pending: Option<Info>,
    /// Set when a page failed part way: the commit it was on is gone from the traversal, so
    /// resuming would silently skip it.
    broken: bool,
}

impl Traversal {
    /// Starts at `tip`, followed through annotated tags to its commit. With `after`, the
    /// traversal ends at the first commit older than that committer time instead of walking
    /// to the root.
    pub(crate) fn start(repo: &gix::Repository, tip: Oid, after: Option<i64>) -> Result<Self> {
        let tip = peel_to_commit(repo, tip)?.id;
        let sorting = match after {
            Some(seconds) => Sorting::ByCommitTimeCutoff {
                order: CommitTimeOrder::NewestFirst,
                seconds,
            },
            None => Sorting::ByCommitTime(CommitTimeOrder::NewestFirst),
        };
        let absent = absent_shallow_parents(repo)?;
        let filter: ParentFilter =
            Box::new(move |id| absent.binary_search(&id.to_owned()).is_err());
        // An unreadable or disabled commit-graph only costs speed: the traversal reads every
        // commit from the object database instead, as gix's own `rev_walk` does.
        let graph = repo.commit_graph_if_enabled().ok().flatten();
        let mut walk_repo = repo.clone();
        walk_repo.object_cache_size(WALK_OBJECT_CACHE_BYTES);
        let simple = Simple::filtered([tip], walk_repo.objects, filter)
            .sorting(sorting)
            .map_err(git("start commit walk"))?
            .commit_graph(graph);
        Ok(Self {
            simple,
            pending: None,
            broken: false,
        })
    }

    /// The next commit, or `None` at the end of history.
    pub(crate) fn next_commit(&mut self) -> Result<Option<Info>> {
        if let Some(info) = self.pending.take() {
            return Ok(Some(info));
        }
        match self.simple.next() {
            Some(info) => info.map(Some).map_err(git("walk commits")),
            None => Ok(None),
        }
    }

    /// Hands `info` back, so the next page starts with it.
    pub(crate) fn put_back(&mut self, info: Info) {
        self.pending = Some(info);
    }

    /// Refuses a traversal whose last page failed part way: it no longer knows which commit
    /// comes next.
    pub(crate) fn ensure_usable(&self) -> Result<()> {
        if self.broken {
            return Err(refused());
        }
        Ok(())
    }

    /// Records that a page failed part way, so every later page refuses.
    pub(crate) fn mark_broken(&mut self) {
        self.broken = true;
    }
}

/// Locks a walk for one page. A walk whose last page panicked refuses, like one whose last
/// page failed (see [`Traversal::ensure_usable`]).
pub(crate) fn lock<T>(walk: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    walk.lock().map_err(|_| refused())
}

fn refused() -> Error {
    Error::Git {
        op: "resume commit walk",
        source: "an earlier page of this walk failed; start a new walk".into(),
    }
}

/// The parents shallow boundary commits list that the clone does not have, sorted. Empty in a
/// complete clone. A parent missing anywhere else is corruption, and the walk still fails on
/// it.
fn absent_shallow_parents(repo: &gix::Repository) -> Result<Vec<ObjectId>> {
    let Some(boundary) = repo.shallow_commits().map_err(git("read shallow file"))? else {
        return Ok(Vec::new());
    };
    let mut absent = Vec::new();
    for id in boundary.iter() {
        let oid = Oid::from_gix(*id);
        let commit = repo.find_commit(*id).map_err(object("read commit", oid))?;
        for parent in commit.parent_ids() {
            if !repo.has_object(parent) {
                absent.push(parent.detach());
            }
        }
    }
    absent.sort();
    absent.dedup();
    Ok(absent)
}
