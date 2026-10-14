import Foundation

/// Caches directory listings by tree id. Git trees are content-addressed, so an entry never
/// goes stale: the same id in another branch or commit is the same directory.
///
/// Bounded by the total number of children it holds, not by tree count, because one flat
/// directory can list a hundred thousand entries: past `childLimit`, the least recently used
/// listings are evicted. A listing larger than the limit on its own is returned uncached.
public final class TreeStore: @unchecked Sendable {
    /// About 20 MB of listings at roughly 200 bytes per entry.
    public static let defaultChildLimit = 100_000

    public let childLimit: Int
    private var entries: [Oid: (children: [TreeEntry], stamp: UInt64)] = [:]
    /// Tree ids by their last use; the smallest stamp present is the least recently used.
    private var recency: [UInt64: Oid] = [:]
    private var nextStamp: UInt64 = 0
    /// No stamp below this one is in `recency`, so eviction resumes its scan here.
    private var oldestStamp: UInt64 = 0
    private var children = 0
    private let lock = NSLock()

    public init(childLimit: Int = TreeStore.defaultChildLimit) {
        precondition(childLimit > 0, "a TreeStore needs room for at least one entry")
        self.childLimit = childLimit
    }

    /// The cached listing of `tree`, marking it recently used, or `nil` when it is not held.
    public func cached(_ tree: Oid) -> [TreeEntry]? {
        lock.withLock {
            guard let entry = entries[tree] else { return nil }
            entries[tree] = (entry.children, touch(tree, previous: entry.stamp))
            return entry.children
        }
    }

    /// The listing of `tree`, from the cache or through `read`, which is then remembered and
    /// may evict older listings. The lock is not held across `read`, so two callers may both
    /// read the same tree once.
    public func entries(of tree: Oid, read: (Oid) throws -> [TreeEntry]) rethrows -> [TreeEntry] {
        if let cached = cached(tree) { return cached }
        let listing = try read(tree)
        store(listing, for: tree)
        return listing
    }

    /// Number of trees held.
    public var count: Int {
        lock.withLock { entries.count }
    }

    /// Number of children across every tree held; never above `childLimit`.
    public var childCount: Int {
        lock.withLock { children }
    }

    public func removeAll() {
        lock.withLock {
            entries.removeAll()
            recency.removeAll()
            oldestStamp = nextStamp
            children = 0
        }
    }

    private func store(_ listing: [TreeEntry], for tree: Oid) {
        lock.withLock {
            let previous = entries[tree]
            if let previous { children -= previous.children.count }
            entries[tree] = (listing, touch(tree, previous: previous?.stamp))
            children += listing.count
            evictPastLimit()
        }
    }

    /// Gives `tree` the newest stamp. Call with the lock held.
    private func touch(_ tree: Oid, previous: UInt64?) -> UInt64 {
        if let previous { recency.removeValue(forKey: previous) }
        let stamp = nextStamp
        nextStamp += 1
        recency[stamp] = tree
        return stamp
    }

    /// Drops the least recently used listings until the total fits. Call with the lock held.
    private func evictPastLimit() {
        while children > childLimit, oldestStamp < nextStamp {
            let stamp = oldestStamp
            oldestStamp += 1
            guard let tree = recency.removeValue(forKey: stamp),
                let evicted = entries.removeValue(forKey: tree)
            else { continue }
            children -= evicted.children.count
        }
    }
}
