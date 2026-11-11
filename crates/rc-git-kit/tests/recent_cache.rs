//! The `RecentCache` bound the browser model's per-tip and per-root caches rely on: reads and
//! inserts mark a key used, and an insert past the limit evicts the least recently used.

#![expect(
    clippy::unwrap_used,
    reason = "the helper builds a fixed non-zero limit"
)]

use std::num::NonZeroUsize;

use rc_git_kit::recent_cache::RecentCache;

fn cache(limit: usize) -> RecentCache<&'static str, u32> {
    RecentCache::new(NonZeroUsize::new(limit).unwrap())
}

#[test]
fn an_insert_past_the_limit_evicts_the_least_recently_used() {
    let mut cache = cache(2);
    cache.insert("a", 1);
    cache.insert("b", 2);
    cache.insert("c", 3);
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.value(&"a"), None);
    assert_eq!(cache.value(&"b"), Some(&2));
    assert_eq!(cache.value(&"c"), Some(&3));
}

#[test]
fn a_read_marks_the_key_used() {
    let mut cache = cache(2);
    cache.insert("a", 1);
    cache.insert("b", 2);
    assert_eq!(cache.value(&"a"), Some(&1));
    cache.insert("c", 3);
    assert_eq!(cache.value(&"b"), None, "b was the least recently used");
    assert_eq!(cache.value(&"a"), Some(&1));
}

#[test]
fn replacing_a_value_marks_it_used_and_keeps_one_entry() {
    let mut cache = cache(2);
    cache.insert("a", 1);
    cache.insert("b", 2);
    cache.insert("a", 10);
    assert_eq!(cache.len(), 2);
    cache.insert("c", 3);
    assert_eq!(cache.value(&"a"), Some(&10));
    assert_eq!(cache.value(&"b"), None);
}

#[test]
fn remove_and_clear_drop_entries_and_their_order() {
    let mut cache = cache(2);
    cache.insert("a", 1);
    cache.insert("b", 2);
    assert_eq!(cache.remove(&"a"), Some(1));
    assert_eq!(cache.remove(&"a"), None);
    cache.insert("c", 3);
    assert_eq!(cache.len(), 2, "the removed key no longer holds a slot");
    cache.clear();
    assert!(cache.is_empty());
    cache.insert("d", 4);
    assert_eq!(cache.value(&"d"), Some(&4));
}
