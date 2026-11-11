//! Port of `TreeStore.swift`: directory listings cached by tree id. Git trees are
//! content-addressed, so an entry never goes stale: the same id in another branch or commit is
//! the same directory.
//!
//! Bounded by the total number of children it holds, not by tree count, because one flat
//! directory can list a hundred thousand entries: past the child limit, the least recently
//! used listings are evicted. A listing larger than the limit on its own is returned uncached.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rc_git_core::{Oid, TreeEntry};

/// One cached listing, shared with every caller that reads it.
pub type Listing = Arc<Vec<TreeEntry>>;

/// About 20 MB of listings at roughly 200 bytes per entry.
pub const DEFAULT_CHILD_LIMIT: NonZeroUsize = match NonZeroUsize::new(100_000) {
    Some(limit) => limit,
    None => NonZeroUsize::MIN,
};

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<Oid, (Listing, u64)>,
    /// Tree ids by their last use; the smallest stamp present is the least recently used.
    recency: HashMap<u64, Oid>,
    next_stamp: u64,
    /// No stamp below this one is in `recency`, so eviction resumes its scan here.
    oldest_stamp: u64,
    children: usize,
}

/// Directory listings by tree id, safe to share between threads.
#[derive(Debug)]
pub struct TreeStore {
    child_limit: usize,
    inner: Mutex<Inner>,
}

impl Default for TreeStore {
    fn default() -> Self {
        Self::new(DEFAULT_CHILD_LIMIT)
    }
}

impl TreeStore {
    /// A store that holds at most `child_limit` children across its listings.
    #[must_use]
    pub fn new(child_limit: NonZeroUsize) -> Self {
        Self {
            child_limit: child_limit.get(),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The most children the store holds at once.
    #[must_use]
    pub fn child_limit(&self) -> usize {
        self.child_limit
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cached listing of `tree`, marking it recently used, or `None` when it is not held.
    #[must_use]
    pub fn cached(&self, tree: Oid) -> Option<Listing> {
        let mut inner = self.lock();
        let (listing, stamp) = inner.entries.get(&tree).cloned()?;
        let stamp = inner.touch(tree, Some(stamp));
        inner.entries.insert(tree, (Arc::clone(&listing), stamp));
        Some(listing)
    }

    /// The listing of `tree`, from the cache or through `read`, which is then remembered and
    /// may evict older listings. The lock is not held across `read`, so two callers may both
    /// read the same tree once.
    ///
    /// # Errors
    /// Whatever `read` returns; nothing is stored then.
    pub fn entries<E>(
        &self,
        tree: Oid,
        read: impl FnOnce(Oid) -> Result<Vec<TreeEntry>, E>,
    ) -> Result<Listing, E> {
        if let Some(cached) = self.cached(tree) {
            return Ok(cached);
        }
        let listing = Arc::new(read(tree)?);
        self.store(Arc::clone(&listing), tree);
        Ok(listing)
    }

    /// Number of trees held.
    #[must_use]
    pub fn count(&self) -> usize {
        self.lock().entries.len()
    }

    /// Number of children across every tree held; never above the child limit.
    #[must_use]
    pub fn child_count(&self) -> usize {
        self.lock().children
    }

    /// Drops every listing.
    pub fn remove_all(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.recency.clear();
        inner.oldest_stamp = inner.next_stamp;
        inner.children = 0;
    }

    fn store(&self, listing: Listing, tree: Oid) {
        let mut inner = self.lock();
        let previous = inner
            .entries
            .get(&tree)
            .map(|(old, stamp)| (old.len(), *stamp));
        if let Some((count, _)) = previous {
            inner.children -= count;
        }
        let stamp = inner.touch(tree, previous.map(|(_, stamp)| stamp));
        inner.children += listing.len();
        inner.entries.insert(tree, (listing, stamp));
        inner.evict_past(self.child_limit);
    }
}

impl Inner {
    /// Gives `tree` the newest stamp.
    fn touch(&mut self, tree: Oid, previous: Option<u64>) -> u64 {
        if let Some(previous) = previous {
            self.recency.remove(&previous);
        }
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        self.recency.insert(stamp, tree);
        stamp
    }

    /// Drops the least recently used listings until the total fits.
    fn evict_past(&mut self, limit: usize) {
        while self.children > limit && self.oldest_stamp < self.next_stamp {
            let stamp = self.oldest_stamp;
            self.oldest_stamp += 1;
            let Some(tree) = self.recency.remove(&stamp) else {
                continue;
            };
            if let Some((evicted, _)) = self.entries.remove(&tree) {
                self.children -= evicted.len();
            }
        }
    }
}
