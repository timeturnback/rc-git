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
    func log(tip: Oid, skip: Int, limit: Int) throws -> [CommitSummary]
    func commit(_ oid: Oid) throws -> CommitSummary
    func commitDetails(_ oid: Oid) throws -> CommitDetail
    func pathHistory(tip: Oid, path: String, scanSkip: Int, limit: Int, maxScan: Int) throws
        -> CommitPage
    func searchCommits(tip: Oid, filter: CommitFilter, scanSkip: Int, limit: Int, maxScan: Int)
        throws -> CommitPage
    func diffTrees(from: Oid?, to: Oid) throws -> [TreeChange]
    func diffBlobs(old: Oid?, new: Oid?, limit: UInt64, context: UInt32) throws -> BlobDiff
    func blame(tip: Oid, path: String) throws -> [BlameRange]
    func worktreeEntries(tree: Oid, dir: String) throws -> WorktreeListing
    func worktreeFile(path: String, limit: UInt64) throws -> BlobContent
    func worktreeStatus(tree: Oid, maxScan: Int) throws -> StatusReport
    func worktreeFiles(tree: Oid, maxScan: Int) throws -> WorktreeFileList
    func diffWorktreeFile(path: String, old: Oid?, limit: UInt64, context: UInt32) throws
        -> BlobDiff
    func stashList() throws -> [StashEntry]
}
