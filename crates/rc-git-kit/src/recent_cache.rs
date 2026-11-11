//! The `RecentCache` from `BrowserModel.swift`: a map that keeps only its most recently used
//! entries. The browser model bounds its per-tip history pages and its per-root path indexes
//! with it; every key is content-addressed, so an entry never goes stale, it only ages out.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::num::NonZeroUsize;

/// A map that keeps only its `limit` most recently used entries: a read or an insert marks the
/// key used, and an insert past the limit evicts the least recently used.
#[derive(Debug, Clone)]
pub struct RecentCache<K, V> {
    limit: usize,
    values: HashMap<K, V>,
    /// Least recently used first.
    order: VecDeque<K>,
}

impl<K: Eq + Hash + Clone, V> RecentCache<K, V> {
    /// An empty cache holding at most `limit` entries.
    #[must_use]
    pub fn new(limit: NonZeroUsize) -> Self {
        Self {
            limit: limit.get(),
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Entries held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether nothing is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The value for `key`, marking it the most recently used.
    pub fn value(&mut self, key: &K) -> Option<&V> {
        if !self.values.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.values.get(key)
    }

    /// Stores `value` under `key` as the most recently used, evicting the least recently used
    /// entries past the limit.
    pub fn insert(&mut self, key: K, value: V) {
        if self.values.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        } else {
            self.touch(&key);
        }
        while self.order.len() > self.limit {
            if let Some(evicted) = self.order.pop_front() {
                self.values.remove(&evicted);
            }
        }
    }

    /// Drops `key`, returning its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let value = self.values.remove(key)?;
        self.order.retain(|held| held != key);
        Some(value)
    }

    /// Drops every entry.
    pub fn clear(&mut self) {
        self.values.clear();
        self.order.clear();
    }

    fn touch(&mut self, key: &K) {
        if let Some(index) = self.order.iter().position(|held| held == key)
            && let Some(moved) = self.order.remove(index)
        {
            self.order.push_back(moved);
        }
    }
}
