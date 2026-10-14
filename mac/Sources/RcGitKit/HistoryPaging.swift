import Foundation

/// A log cursor plus how many commits it has handed out, so the model can tell whether it
/// still stands where a tip's cached pages end. A page whose result the model dropped as stale
/// moves the cursor past the cache; the next page then sees the mismatch and opens a new one.
final class LogPager: @unchecked Sendable {
    private let lock = NSLock()
    private let cursor: any LogCursorReading
    /// Commits handed out so far, or `nil` once a page failed and the place is lost.
    private var position: Int?

    private init(cursor: any LogCursorReading) {
        self.cursor = cursor
        self.position = 0
    }

    /// The `limit` commits after the first `start` reachable from `tip`, and the pager that
    /// now stands after them. Resumes `pager` when it stands at `start`; otherwise opens a new
    /// cursor and passes over the first `start` commits again.
    static func page(
        _ pager: LogPager?, repository: any RepositoryReading, tip: Oid, from start: Int,
        limit: Int
    ) throws -> (LogPager, [CommitSummary]) {
        if let pager, let page = try pager.next(from: start, limit: limit) {
            return (pager, page)
        }
        let fresh = LogPager(cursor: try repository.logCursor(tip: tip))
        if start > 0 {
            _ = try fresh.next(from: 0, limit: start)
        }
        guard let page = try fresh.next(from: start, limit: limit) else {
            // The history ended before `start`: nothing follows it.
            return (fresh, [])
        }
        return (fresh, page)
    }

    /// The next `limit` commits, or `nil` when the cursor does not stand at `start`.
    private func next(from start: Int, limit: Int) throws -> [CommitSummary]? {
        try lock.withLock {
            guard position == start else { return nil }
            position = nil
            let page = try cursor.next(limit: limit)
            position = start + page.count
            return page
        }
    }
}

/// A filtered history walk plus how many commits it has examined, so the model can tell
/// whether it still stands where the cached pages of a path or search end. Like `LogPager`,
/// a mismatch opens a new walk.
final class HistoryPager: @unchecked Sendable {
    private let lock = NSLock()
    private let walk: any HistoryWalkReading
    /// Commits examined so far, or `nil` once a page failed and the place is lost.
    private var position: Int?

    private init(walk: any HistoryWalkReading) {
        self.walk = walk
        self.position = 0
    }

    /// The next page of the walk over `tip` and `filter` after its first `start` examined
    /// commits, and the pager that now stands after it. Resumes `pager` when it stands at
    /// `start`; otherwise opens a new walk and examines the first `start` commits again.
    static func page(
        _ pager: HistoryPager?, repository: any RepositoryReading, tip: Oid,
        filter: CommitFilter, from start: Int, limit: Int, maxScan: Int
    ) throws -> (HistoryPager, CommitPage) {
        if let pager, let page = try pager.next(from: start, limit: limit, maxScan: maxScan) {
            return (pager, page)
        }
        let fresh = HistoryPager(walk: try repository.historyWalk(tip: tip, filter: filter))
        if start > 0 {
            _ = try fresh.next(from: 0, limit: Int.max, maxScan: start)
        }
        guard let page = try fresh.next(from: start, limit: limit, maxScan: maxScan) else {
            // The history ended before `start`: nothing follows it.
            return (fresh, CommitPage(commits: [], scanned: 0, complete: true))
        }
        return (fresh, page)
    }

    /// The next page, or `nil` when the walk does not stand at `start`.
    private func next(from start: Int, limit: Int, maxScan: Int) throws -> CommitPage? {
        try lock.withLock {
            guard position == start else { return nil }
            position = nil
            let page = try walk.next(limit: limit, maxScan: maxScan)
            position = start + page.scanned
            return page
        }
    }
}
