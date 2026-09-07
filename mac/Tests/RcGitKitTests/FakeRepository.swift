import Foundation
import RcGitKit

/// An in-memory repository for view-model tests. Each call can be delayed so tests can race
/// two loads and check which result wins.
final class FakeRepository: RepositoryReading, @unchecked Sendable {
    let lock = NSLock()
    var trees: [Oid: [TreeEntry]] = [:]
    var blobs: [Oid: BlobContent] = [:]
    var commits: [Oid: [CommitSummary]] = [:]
    var summaries: [Oid: CommitSummary] = [:]
    var details: [Oid: CommitDetail] = [:]
    /// Per "tip.hex:path", the full matching history. The fake treats every stored commit as
    /// one scanned commit, so `scanSkip` indexes into this list.
    var pathHistories: [String: [CommitSummary]] = [:]
    var pathHistoryCalls = 0
    var resolves: [String: Oid] = [:]
    var searchCalls = 0
    var treeDiffs: [Oid: [TreeChange]] = [:]
    var blobDiffs: [String: BlobDiff] = [:]
    var blames: [String: [BlameRange]] = [:]
    var worktreeListings: [String: WorktreeListing] = [:]
    var worktreeFiles: [String: BlobContent] = [:]
    /// The full-repo status walk result, replaced by tests as the "disk" changes.
    var statusReport = StatusReport(entries: [], scanned: 0, truncated: false)
    var statusCalls = 0
    var failNextStatus = false
    /// Per path, the worktree-vs-blob diff.
    var worktreeDiffs: [String: BlobDiff] = [:]
    var stashes: [StashEntry] = []
    /// Slept inside `worktreeStatus`, for racing loads.
    var statusDelay: TimeInterval = 0
    var listing = RefListing(refs: [])
    var headValue = Head(refName: nil, target: nil)
    var treeOfMap: [Oid: Oid] = [:]
    var delays: [Oid: TimeInterval] = [:]
    var treeEntryCalls = 0
    var failNextTree = false
    var failNextRefs = false

    func head() throws -> Head { lock.withLock { headValue } }

    func refs() throws -> RefListing {
        let (result, fail) = lock.withLock { () -> (RefListing, Bool) in
            let fail = failNextRefs
            failNextRefs = false
            return (listing, fail)
        }
        if fail { throw RepositoryError("cannot list refs: transient lock") }
        return result
    }

    func resolve(_ spec: String) throws -> Oid {
        guard let found = lock.withLock({ resolves[spec] }) else {
            throw RepositoryError("'\(spec)' is not a revision in this repository")
        }
        return found
    }

    func resolveTree(_ spec: String) throws -> Oid {
        throw RepositoryError("'\(spec)' is not a revision in this repository")
    }

    func treeOf(_ oid: Oid) throws -> Oid {
        let (mapped, delay) = lock.withLock { (treeOfMap[oid], delays[oid] ?? 0) }
        if delay > 0 { Thread.sleep(forTimeInterval: delay) }
        guard let mapped else { throw RepositoryError("object \(oid) is a blob, expected a tree") }
        return mapped
    }

    func treeEntries(_ tree: Oid) throws -> [TreeEntry] {
        let (entries, fail, delay) = lock.withLock { () -> ([TreeEntry]?, Bool, TimeInterval) in
            treeEntryCalls += 1
            let fail = failNextTree
            failNextTree = false
            return (trees[tree], fail, delays[tree] ?? 0)
        }
        if delay > 0 { Thread.sleep(forTimeInterval: delay) }
        if fail { throw RepositoryError("read tree: object \(tree) is missing or unreadable") }
        guard let entries else {
            throw RepositoryError("read tree: object \(tree) is missing or unreadable")
        }
        return entries
    }

    func readBlob(_ oid: Oid, limit: UInt64) throws -> BlobContent {
        let (content, delay) = lock.withLock { (blobs[oid], delays[oid] ?? 0) }
        if delay > 0 { Thread.sleep(forTimeInterval: delay) }
        guard let content else {
            throw RepositoryError("read blob: object \(oid) is missing or unreadable")
        }
        if case let .text(data, _) = content, UInt64(data.count) > limit {
            return .tooLarge(size: UInt64(data.count), limit: limit)
        }
        return content
    }

    func log(tip: Oid, skip: Int, limit: Int) throws -> [CommitSummary] {
        let (list, delay) = lock.withLock { (commits[tip], delays[tip] ?? 0) }
        if delay > 0 { Thread.sleep(forTimeInterval: delay) }
        guard let list else {
            throw RepositoryError("start commit walk: object \(tip) is missing or unreadable")
        }
        return Array(list.dropFirst(skip).prefix(limit))
    }

    func commit(_ oid: Oid) throws -> CommitSummary {
        let found = lock.withLock {
            summaries[oid] ?? commits.values.joined().first(where: { $0.oid == oid })
        }
        guard let found else {
            throw RepositoryError("read commit: object \(oid) is missing or unreadable")
        }
        return found
    }

    func commitDetails(_ oid: Oid) throws -> CommitDetail {
        if let found = lock.withLock({ details[oid] }) { return found }
        let summary = (try? commit(oid)) ?? commitSummary(oid)
        return CommitDetail(
            summary: summary, body: "", committerName: summary.authorName,
            committerEmail: summary.authorEmail, committerTime: summary.authorTime,
            committerOffset: summary.authorOffset, tree: oid)
    }

    func pathHistory(tip: Oid, path: String, scanSkip: Int, limit: Int, maxScan: Int) throws
        -> CommitPage
    {
        let found = lock.withLock { () -> [CommitSummary]? in
            pathHistoryCalls += 1
            return pathHistories[FakeRepository.pathKey(tip: tip, path: path)]
        }
        guard let found else {
            throw RepositoryError("start commit walk: object \(tip) is missing or unreadable")
        }
        let slice = found.dropFirst(scanSkip).prefix(min(limit, maxScan))
        return CommitPage(
            commits: Array(slice), scanned: slice.count,
            complete: scanSkip + slice.count >= found.count)
    }

    static func pathKey(tip: Oid, path: String) -> String {
        "\(tip.hex):\(path)"
    }

    /// Filters the commits stored for `tip` by title and author, one scanned per stored
    /// commit, so `scanSkip` indexes the unfiltered list.
    func searchCommits(tip: Oid, filter: CommitFilter, scanSkip: Int, limit: Int, maxScan: Int)
        throws -> CommitPage
    {
        let found = lock.withLock { () -> [CommitSummary]? in
            searchCalls += 1
            return commits[tip]
        }
        guard let found else {
            throw RepositoryError("start commit walk: object \(tip) is missing or unreadable")
        }
        let slice = found.dropFirst(scanSkip).prefix(maxScan)
        var matched: [CommitSummary] = []
        var scanned = 0
        for commit in slice {
            scanned += 1
            let messageOK = filter.message.map { commit.title.localizedCaseInsensitiveContains($0) }
            let authorOK = filter.author.map {
                commit.authorName.localizedCaseInsensitiveContains($0)
                    || commit.authorEmail.localizedCaseInsensitiveContains($0)
            }
            if messageOK ?? true, authorOK ?? true {
                matched.append(commit)
                if matched.count >= limit { break }
            }
        }
        return CommitPage(
            commits: matched, scanned: scanned,
            complete: scanSkip + scanned >= found.count)
    }

    func diffTrees(from: Oid?, to: Oid) throws -> [TreeChange] {
        let found = lock.withLock { treeDiffs[to] }
        guard let found else {
            throw RepositoryError("read tree: object \(to) is missing or unreadable")
        }
        return found
    }

    func diffBlobs(old: Oid?, new: Oid?, limit: UInt64, context: UInt32) throws -> BlobDiff {
        let key = FakeRepository.diffKey(old: old, new: new)
        let found = lock.withLock { blobDiffs[key] }
        guard let found else {
            throw RepositoryError("read blob: a diff side is missing or unreadable")
        }
        return found
    }

    func blame(tip: Oid, path: String) throws -> [BlameRange] {
        let found = lock.withLock { blames[path] }
        guard let found else {
            throw RepositoryError("blame file: '\(path)' does not exist at \(tip)")
        }
        return found
    }

    static func diffKey(old: Oid?, new: Oid?) -> String {
        "\(old?.hex ?? "none")..\(new?.hex ?? "none")"
    }

    func worktreeEntries(tree: Oid, dir: String) throws -> WorktreeListing {
        let found = lock.withLock { worktreeListings[dir] }
        guard let found else {
            throw RepositoryError("list working tree: '\(dir)': cannot be read")
        }
        return found
    }

    func worktreeFile(path: String, limit: UInt64) throws -> BlobContent {
        let found = lock.withLock { worktreeFiles[path] }
        guard let found else {
            throw RepositoryError("read working tree file: '\(path)': missing")
        }
        if case let .text(data, _) = found, UInt64(data.count) > limit {
            return .tooLarge(size: UInt64(data.count), limit: limit)
        }
        return found
    }

    func worktreeStatus(tree: Oid, maxScan: Int) throws -> StatusReport {
        let (report, delay, fail) = lock.withLock { () -> (StatusReport, TimeInterval, Bool) in
            statusCalls += 1
            let fail = failNextStatus
            failNextStatus = false
            return (statusReport, statusDelay, fail)
        }
        if delay > 0 { Thread.sleep(forTimeInterval: delay) }
        if fail { throw RepositoryError("scan working tree: transient failure") }
        return report
    }

    func diffWorktreeFile(path: String, old: Oid?, limit: UInt64, context: UInt32) throws
        -> BlobDiff
    {
        let found = lock.withLock { worktreeDiffs[path] }
        guard let found else {
            throw RepositoryError("read working tree file: '\(path)': missing")
        }
        return found
    }

    func stashList() throws -> [StashEntry] {
        lock.withLock { stashes }
    }
}

func commitSummary(_ id: Oid, parents: [Oid] = [], title: String = "change") -> CommitSummary {
    CommitSummary(
        oid: id, parents: parents, authorName: "Test Author", authorEmail: "author@example.com",
        authorTime: 1_704_067_200, authorOffset: 0, title: title)
}

func oid(_ seed: UInt8) -> Oid {
    Oid(bytes: [UInt8](repeating: seed, count: 20))
}

func textBlob(_ string: String) -> BlobContent {
    let bytes = Array(string.utf8)
    var starts: [UInt32] = bytes.isEmpty ? [] : [0]
    for (index, byte) in bytes.enumerated()
    where byte == UInt8(ascii: "\n") && index + 1 < bytes.count {
        starts.append(UInt32(index + 1))
    }
    return .text(data: bytes, lineStarts: starts)
}
