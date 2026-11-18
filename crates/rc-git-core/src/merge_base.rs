//! The merge base of two commits: their best common ancestor, found the way `git merge-base`
//! finds it, with the walk bounded so no call holds a pool handle through a long scan.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use gix::ObjectId;

use crate::error::{Result, object};
use crate::log::peel_to_commit;
use crate::oid::Oid;
use crate::repo::Repository;
use crate::walk::absent_shallow_parents;

/// What [`Repository::merge_base`] found. Every answer says how many commits it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MergeBase {
    /// The best common ancestor: no other common ancestor descends from it. With several (a
    /// criss-cross merge) it is the one with the newest committer time.
    Found {
        /// The common ancestor.
        base: Oid,
        /// Commits read.
        scanned: usize,
    },
    /// The walk ran to its end without meeting a common ancestor: the histories are unrelated.
    Unrelated {
        /// Commits read.
        scanned: usize,
    },
    /// The walk stopped before it could tell: it read `max_scan` commits, or reached the edge
    /// of a shallow clone on a path that still mattered. Nothing is guessed.
    Incomplete {
        /// Commits read.
        scanned: usize,
    },
}

/// Reached from the first commit.
const ONE: u8 = 1;
/// Reached from the second commit.
const TWO: u8 = 2;
/// Reached from a common ancestor: it cannot be a best one.
const STALE: u8 = 4;
/// Recorded as a candidate.
const RESULT: u8 = 8;

/// One commit waiting in the walk, newest committer time first; among equal times, the one
/// queued first.
#[derive(Debug, PartialEq, Eq)]
struct Queued {
    time: i64,
    order: u64,
    id: ObjectId,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time
            .cmp(&other.time)
            .then_with(|| other.order.cmp(&self.order))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The walk's state: git's paint-down-to-common, over flags kept per commit. `live` counts the
/// queued entries whose commit is not stale, so the walk knows when to stop without scanning
/// its queue.
struct Paint {
    flags: HashMap<ObjectId, u8>,
    queued: HashMap<ObjectId, usize>,
    queue: BinaryHeap<Queued>,
    live: usize,
    order: u64,
    candidates: Vec<ObjectId>,
}

impl Paint {
    fn new() -> Self {
        Self {
            flags: HashMap::new(),
            queued: HashMap::new(),
            queue: BinaryHeap::new(),
            live: 0,
            order: 0,
            candidates: Vec::new(),
        }
    }

    fn flags(&self, id: &ObjectId) -> u8 {
        self.flags.get(id).copied().unwrap_or(0)
    }

    /// Adds `add` to `id`'s flags and queues it. A commit turning stale takes every entry it
    /// already has in the queue out of the live count.
    fn push(&mut self, id: ObjectId, time: i64, add: u8) {
        let before = self.flags(&id);
        let after = before | add;
        self.flags.insert(id, after);
        let waiting = self.queued.entry(id).or_insert(0);
        if before & STALE == 0 && after & STALE != 0 {
            self.live -= *waiting;
        }
        *waiting += 1;
        if after & STALE == 0 {
            self.live += 1;
        }
        self.order += 1;
        self.queue.push(Queued {
            time,
            order: self.order,
            id,
        });
    }

    /// The next commit while any queued one could still be a best common ancestor.
    fn pop(&mut self) -> Option<ObjectId> {
        if self.live == 0 {
            return None;
        }
        let queued = self.queue.pop()?;
        if let Some(waiting) = self.queued.get_mut(&queued.id) {
            *waiting -= 1;
        }
        if self.flags(&queued.id) & STALE == 0 {
            self.live -= 1;
        }
        Some(queued.id)
    }

    /// Records `id`, reached from both sides, as a candidate.
    fn record(&mut self, id: ObjectId) {
        let flags = self.flags.entry(id).or_insert(0);
        if *flags & RESULT == 0 {
            *flags |= RESULT;
            self.candidates.push(id);
        }
    }

    /// The best candidate: the first found that no later candidate reached.
    fn best(&self) -> Option<ObjectId> {
        self.candidates
            .iter()
            .find(|id| self.flags(id) & STALE == 0)
            .copied()
    }
}

impl Repository {
    /// The merge base of `one` and `two` (each followed through annotated tags to its commit):
    /// the common ancestor a three-dot diff `one...two` starts from. The walk goes newest
    /// committer time first and reads at most `max_scan` commits; a walk that needs more, or
    /// that reaches the edge of a shallow clone on a path that still matters, answers
    /// [`MergeBase::Incomplete`] rather than a guess. When one commit is an ancestor of the
    /// other, it is the merge base.
    ///
    /// # Errors
    /// [`Error::Object`](crate::Error::Object) when either id or a commit on the way is
    /// missing or cannot be decoded (outside a shallow clone's edge),
    /// [`Error::WrongKind`](crate::Error::WrongKind) when either id is not a commit.
    pub fn merge_base(&self, one: Oid, two: Oid, max_scan: usize) -> Result<MergeBase> {
        self.with(|repo| {
            let one = peel_to_commit(repo, one)?;
            let two = peel_to_commit(repo, two)?;
            if one.id == two.id {
                return Ok(MergeBase::Found {
                    base: Oid::from_gix(one.id),
                    scanned: 0,
                });
            }
            let absent = absent_shallow_parents(repo)?;
            let mut paint = Paint::new();
            paint.push(one.id, commit_time(&one)?, ONE);
            paint.push(two.id, commit_time(&two)?, TWO);
            paint_down(repo, &mut paint, &absent, max_scan)
        })
    }
}

/// Runs the walk to its answer.
fn paint_down(
    repo: &gix::Repository,
    paint: &mut Paint,
    absent: &[ObjectId],
    max_scan: usize,
) -> Result<MergeBase> {
    let mut scanned = 0;
    let mut cut = false;
    while let Some(id) = paint.pop() {
        if scanned == max_scan {
            return Ok(MergeBase::Incomplete { scanned });
        }
        scanned += 1;
        let mut flags = paint.flags(&id) & (ONE | TWO | STALE);
        if flags == ONE | TWO {
            paint.record(id);
            flags |= STALE;
        }
        let commit = repo
            .find_commit(id)
            .map_err(object("read commit", Oid::from_gix(id)))?;
        for parent in commit.parent_ids() {
            let parent = parent.detach();
            if absent.binary_search(&parent).is_ok() {
                cut |= flags & STALE == 0;
                continue;
            }
            if paint.flags(&parent) & flags == flags {
                continue;
            }
            let parent_commit = repo
                .find_commit(parent)
                .map_err(object("read commit", Oid::from_gix(parent)))?;
            paint.push(parent, commit_time(&parent_commit)?, flags);
        }
    }
    Ok(match (paint.best(), cut) {
        (_, true) => MergeBase::Incomplete { scanned },
        (Some(base), false) => MergeBase::Found {
            base: Oid::from_gix(base),
            scanned,
        },
        (None, false) => MergeBase::Unrelated { scanned },
    })
}

/// The commit's committer time in seconds, which orders the walk.
fn commit_time(commit: &gix::Commit<'_>) -> Result<i64> {
    let time = commit
        .time()
        .map_err(object("read commit time", Oid::from_gix(commit.id)))?;
    Ok(time.seconds)
}
