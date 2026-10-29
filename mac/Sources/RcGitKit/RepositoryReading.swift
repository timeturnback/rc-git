/// The read operations the view models depend on. `Repository` is the real implementation;
/// tests inject a fake. Every method may be called from any thread.
public protocol RepositoryReading: Sendable {
    /// The working directory, or `nil` for a bare repository.
    var workingDirectory: String? { get }
    func head() throws -> Head
    func refs() throws -> RefListing
    func resolve(_ spec: String) throws -> Oid
    func resolveTree(_ spec: String) throws -> Oid
    func treeOf(_ oid: Oid) throws -> Oid
    func treeEntries(_ tree: Oid) throws -> [TreeEntry]
    func readBlob(_ oid: Oid, limit: UInt64) throws -> BlobContent
    func logCursor(tip: Oid) throws -> any LogCursorReading
    func commit(_ oid: Oid) throws -> CommitSummary
    func commitDetails(_ oid: Oid) throws -> CommitDetail
    func historyWalk(tip: Oid, filter: CommitFilter) throws -> any HistoryWalkReading
    func diffTrees(from: Oid?, to: Oid) throws -> [TreeChange]
    func diffBlobs(old: Oid?, new: Oid?, limit: UInt64, context: UInt32) throws -> BlobDiff
    func blame(tip: Oid, path: String, limit: UInt64, since: Int64?) throws -> Blame
    func worktreeEntries(tree: Oid, dir: String) throws -> WorktreeListing
    func worktreeFile(path: String, limit: UInt64) throws -> BlobContent
    func worktreeStatus(tree: Oid?, maxScan: Int) throws -> StatusReport
    func worktreeFiles(tree: Oid, maxScan: Int) throws -> WorktreeFileList
    func diffWorktreeFile(path: String, old: Oid?, limit: UInt64, context: UInt32) throws
        -> BlobDiff
    func stashList() throws -> [StashEntry]
    func searchBlobs(_ oids: [Oid], query: String, limits: ContentSearchLimits) throws
        -> [FileSearchResult]
    func searchWorktreeFiles(_ paths: [String], query: String, limits: ContentSearchLimits)
        throws -> [FileSearchResult]
}

/// A walk over the commits reachable from one tip, newest first. Each call resumes where the
/// previous one stopped, so paging to depth N costs N commits in total.
public protocol LogCursorReading: Sendable {
    /// The next at most `limit` commits; fewer only at the end of history. After a failed
    /// call every later one fails too: open a new cursor.
    func next(limit: Int) throws -> [CommitSummary]
}

/// A filtered walk over the commits reachable from one tip, newest first. Each call resumes
/// where the previous one stopped, so a sparse filter costs one pass over history in total.
public protocol HistoryWalkReading: Sendable {
    /// At most `limit` matches from at most `maxScan` commits examined; loop until the page
    /// is `complete`. After a failed call every later one fails too: open a new walk.
    func next(limit: Int, maxScan: Int) throws -> CommitPage
}
