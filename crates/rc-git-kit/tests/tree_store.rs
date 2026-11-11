//! `TreeStore`: listings cached by tree id, bounded by the children they hold. The store's job
//! is saving reads, so the read counter carries the assertions.
#![expect(clippy::unwrap_used, reason = "the helpers build fixed valid inputs")]

use std::cell::Cell;
use std::num::NonZeroUsize;

use rc_git_core::{EntryKind, Oid, TreeEntry};
use rc_git_kit::fake::oid;
use rc_git_kit::tree_store::{DEFAULT_CHILD_LIMIT, TreeStore};

fn listing(count: usize) -> Vec<TreeEntry> {
    (0..count)
        .map(|index| TreeEntry {
            name: format!("f{index}").into(),
            oid: oid(0xAA),
            kind: EntryKind::Blob,
        })
        .collect()
}

fn limit(children: usize) -> NonZeroUsize {
    NonZeroUsize::new(children).unwrap()
}

/// Reads `tree` through `store`, counting the reads that reached `reads`.
fn read(store: &TreeStore, tree: Oid, count: usize, reads: &Cell<usize>) -> usize {
    let entries = store
        .entries::<std::convert::Infallible>(tree, |_| {
            reads.set(reads.get() + 1);
            Ok(listing(count))
        })
        .unwrap();
    entries.len()
}

#[test]
fn a_second_read_of_a_tree_is_a_cache_hit() {
    let store = TreeStore::default();
    assert_eq!(store.child_limit(), DEFAULT_CHILD_LIMIT.get());
    let reads = Cell::new(0);
    assert_eq!(read(&store, oid(1), 3, &reads), 3);
    assert_eq!(read(&store, oid(1), 3, &reads), 3);
    assert_eq!(reads.get(), 1);
    assert_eq!(store.count(), 1);
    assert_eq!(store.child_count(), 3);
}

#[test]
fn past_the_child_limit_the_least_recently_used_listing_goes() {
    let store = TreeStore::new(limit(5));
    let reads = Cell::new(0);
    read(&store, oid(1), 2, &reads);
    read(&store, oid(2), 2, &reads);
    // Touch the first, so the second is the least recently used.
    assert!(store.cached(oid(1)).is_some());
    read(&store, oid(3), 2, &reads);
    assert_eq!(store.child_count(), 4);
    assert!(
        store.cached(oid(2)).is_none(),
        "the least recently used listing was evicted"
    );
    assert!(store.cached(oid(1)).is_some());
    assert!(store.cached(oid(3)).is_some());
    assert_eq!(reads.get(), 3);
    read(&store, oid(2), 2, &reads);
    assert_eq!(reads.get(), 4, "an evicted listing is read again");
}

#[test]
fn a_listing_larger_than_the_limit_is_returned_but_not_kept() {
    let store = TreeStore::new(limit(3));
    let reads = Cell::new(0);
    assert_eq!(read(&store, oid(1), 1, &reads), 1);
    assert_eq!(read(&store, oid(9), 10, &reads), 10);
    assert_eq!(
        store.count(),
        0,
        "the huge listing pushed out everything, itself included"
    );
    assert_eq!(store.child_count(), 0);
    assert_eq!(read(&store, oid(9), 10, &reads), 10);
    assert_eq!(reads.get(), 3);
}

#[test]
fn a_failed_read_stores_nothing_and_remove_all_empties_the_store() {
    let store = TreeStore::default();
    let failed = store.entries(oid(1), |_| Err("unreadable"));
    assert_eq!(failed, Err("unreadable"));
    assert_eq!(store.count(), 0);

    let reads = Cell::new(0);
    read(&store, oid(1), 2, &reads);
    store.remove_all();
    assert_eq!(store.count(), 0);
    assert_eq!(store.child_count(), 0);
    read(&store, oid(1), 2, &reads);
    assert_eq!(reads.get(), 2);
    assert_eq!(
        store.child_count(),
        2,
        "the counts restart cleanly after a clear"
    );
}
