import Foundation

/// Caches directory listings by tree id. Git trees are content-addressed, so an entry never
/// goes stale: the same id in another branch or commit is the same directory.
public final class TreeStore: @unchecked Sendable {
    private var entries: [Oid: [TreeEntry]] = [:]
    private let lock = NSLock()

    public init() {}

    public func cached(_ tree: Oid) -> [TreeEntry]? {
        lock.withLock { entries[tree] }
    }

    public func store(_ children: [TreeEntry], for tree: Oid) {
        lock.withLock { entries[tree] = children }
    }

    public var count: Int {
        lock.withLock { entries.count }
    }

    public func removeAll() {
        lock.withLock { entries.removeAll() }
    }
}
