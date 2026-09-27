import CRcGit
import Foundation

/// An open repository, backed by the Rust core. Safe to use from any thread: the core keeps a
/// pool of handles and never blocks one call on another.
public final class Repository: RepositoryReading, @unchecked Sendable {
    /// Default cap for `readBlob`: 8 MiB, matching the core.
    public static let defaultBlobLimit: UInt64 = 8 * 1024 * 1024

    /// Default cap for `worktreeStatus`, matching the path-search precedent.
    public static let defaultStatusScanCap = 200_000

    private let handle: OpaquePointer

    /// Opens a working directory or a `.git` directory.
    public init(path: String) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let handle = rcgit_repo_open(path, &error) else {
            throw Repository.take(error, fallback: "cannot open \(path)")
        }
        self.handle = handle
    }

    deinit {
        rcgit_repo_free(handle)
    }

    /// Version of the Rust library, for sanity checks.
    public static var libraryVersion: String {
        String(cString: rcgit_version())
    }

    /// The `.git` directory.
    public var gitDirectory: String {
        Repository.takeString(rcgit_repo_git_dir(handle)) ?? ""
    }

    /// The working directory, or `nil` for a bare repository.
    public var workingDirectory: String? {
        Repository.takeString(rcgit_repo_work_dir(handle))
    }

    public func head() throws -> Head {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let head = rcgit_head(handle, &error) else {
            throw Repository.take(error, fallback: "cannot read HEAD")
        }
        defer { rcgit_head_free(head) }
        let refName = head.pointee.ref_name.map { String(cString: $0) }
        let target = head.pointee.has_target ? Oid(c: head.pointee.target) : nil
        return Head(refName: refName, target: target)
    }

    /// The repository's mainline branch without `refs/heads/`: what a remote's `HEAD` names,
    /// else whichever of `master` and `main` exists locally. `nil` when nothing names one.
    /// The branch may not exist locally; checking it out reports that by name.
    public func defaultBranch() throws -> String? {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let branch = rcgit_default_branch(handle, &error) else {
            if let error {
                throw Repository.take(error, fallback: "cannot read the default branch")
            }
            return nil
        }
        defer { rcgit_string_free(branch) }
        return String(cString: branch)
    }

    public func refs() throws -> RefListing {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = rcgit_refs(handle, &error) else {
            throw Repository.take(error, fallback: "cannot list references")
        }
        defer { rcgit_ref_list_free(list) }
        var refs: [RefEntry] = []
        refs.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.refs[index]
            refs.append(
                RefEntry(
                    fullName: String(cString: c.full_name),
                    shortName: String(cString: c.short_name),
                    kind: RefKind(c: c.kind),
                    target: Oid(c: c.target)
                ))
        }
        var broken: [BrokenRef] = []
        for index in 0..<list.pointee.broken_len {
            let c = list.pointee.broken[index]
            broken.append(
                BrokenRef(fullName: String(cString: c.full_name), error: String(cString: c.error)))
        }
        return RefListing(refs: refs, broken: broken)
    }

    /// Resolves a revision expression (`main`, `v1.0`, `HEAD~2`, an abbreviated hash).
    public func resolve(_ spec: String) throws -> Oid {
        var out = RcGitOid()
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_resolve(handle, spec, &out, &error) == RCGIT_OK else {
            throw Repository.take(error, fallback: "cannot resolve \(spec)")
        }
        return Oid(c: out)
    }

    public func resolveTree(_ spec: String) throws -> Oid {
        var out = RcGitOid()
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_resolve_tree(handle, spec, &out, &error) == RCGIT_OK else {
            throw Repository.take(error, fallback: "cannot resolve \(spec)")
        }
        return Oid(c: out)
    }

    public func treeOf(_ oid: Oid) throws -> Oid {
        var out = RcGitOid()
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = oid.withC { rcgit_tree_of(handle, $0, &out, &error) }
        guard status == RCGIT_OK else {
            throw Repository.take(error, fallback: "cannot peel \(oid)")
        }
        return Oid(c: out)
    }

    public func treeEntries(_ tree: Oid) throws -> [TreeEntry] {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = tree.withC({ rcgit_tree_entries(handle, $0, &error) }) else {
            throw Repository.take(error, fallback: "cannot read tree \(tree)")
        }
        defer { rcgit_tree_list_free(list) }
        var entries: [TreeEntry] = []
        entries.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            entries.append(TreeEntry(c: list.pointee.entries[index]))
        }
        return entries
    }

    /// The entry at a `/`-separated path below `root`, or `nil` when it does not exist.
    public func entry(at path: String, under root: Oid) throws -> TreeEntry? {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let entry = root.withC({ rcgit_entry_at_path(handle, $0, path, &error) }) else {
            if let error { throw Repository.take(error, fallback: "cannot look up \(path)") }
            return nil
        }
        defer { rcgit_tree_entry_free(entry) }
        return TreeEntry(c: entry.pointee)
    }

    public func readBlob(_ oid: Oid, limit: UInt64 = Repository.defaultBlobLimit) throws
        -> BlobContent
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let blob = oid.withC({ rcgit_blob_read(handle, $0, limit, &error) }) else {
            throw Repository.take(error, fallback: "cannot read blob \(oid)")
        }
        defer { rcgit_blob_free(blob) }
        let c = blob.pointee
        switch c.kind {
        case RcGitBlobKind_Text:
            let data = Array(UnsafeBufferPointer(start: c.data, count: c.data_len))
            let lineStarts = Array(UnsafeBufferPointer(start: c.line_starts, count: c.line_count))
            return .text(data: data, lineStarts: lineStarts, validUTF8: c.valid_utf8)
        case RcGitBlobKind_Binary:
            return .binary(size: c.size)
        default:
            return .tooLarge(size: c.size, limit: c.limit)
        }
    }

    /// Size in bytes, from the object header, without decoding the content.
    public func blobSize(_ oid: Oid) throws -> UInt64 {
        var size: UInt64 = 0
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = oid.withC { rcgit_blob_size(handle, $0, &size, &error) }
        guard status == RCGIT_OK else {
            throw Repository.take(error, fallback: "cannot size blob \(oid)")
        }
        return size
    }

    /// Commits reachable from `tip`, newest first. The walk restarts from `tip` each call.
    public func log(tip: Oid, skip: Int = 0, limit: Int = 100) throws -> [CommitSummary] {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = tip.withC({ rcgit_log(handle, $0, skip, limit, &error) }) else {
            throw Repository.take(error, fallback: "cannot walk history from \(tip)")
        }
        defer { rcgit_commit_list_free(list) }
        var commits: [CommitSummary] = []
        commits.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            commits.append(CommitSummary(c: list.pointee.commits[index]))
        }
        return commits
    }

    /// Commits reachable from `tip` that changed `path`, newest first. Examines at most
    /// `maxScan` commits past `scanSkip`; loop on the returned page's `scanned` until
    /// `complete`.
    public func pathHistory(tip: Oid, path: String, scanSkip: Int, limit: Int, maxScan: Int)
        throws -> CommitPage
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        let page = tip.withC { tipPtr in
            path.withCString { pathPtr in
                rcgit_path_history(handle, tipPtr, pathPtr, scanSkip, limit, maxScan, &error)
            }
        }
        guard let page else {
            throw Repository.take(error, fallback: "cannot walk history of '\(path)'")
        }
        defer { rcgit_commit_page_free(page) }
        return Repository.commitPage(page.pointee)
    }

    /// Commits reachable from `tip` matching every set field of `filter`, newest first.
    /// Pages like `pathHistory`; a set `after` also terminates the walk at that time.
    public func searchCommits(
        tip: Oid, filter: CommitFilter, scanSkip: Int, limit: Int, maxScan: Int
    ) throws -> CommitPage {
        var error: UnsafeMutablePointer<CChar>? = nil
        let page = tip.withC { tipPtr in
            Repository.withOptionalCString(filter.message) { messagePtr in
                Repository.withOptionalCString(filter.author) { authorPtr in
                    rcgit_search_commits(
                        handle, tipPtr, messagePtr, authorPtr,
                        filter.after ?? Int64.min, filter.before ?? Int64.max,
                        scanSkip, limit, maxScan, &error)
                }
            }
        }
        guard let page else {
            throw Repository.take(error, fallback: "cannot search history from \(tip)")
        }
        defer { rcgit_commit_page_free(page) }
        return Repository.commitPage(page.pointee)
    }

    private static func commitPage(_ c: RcGitCommitPage) -> CommitPage {
        var commits: [CommitSummary] = []
        commits.reserveCapacity(c.len)
        for index in 0..<c.len {
            commits.append(CommitSummary(c: c.commits[index]))
        }
        return CommitPage(commits: commits, scanned: c.scanned, complete: c.complete)
    }

    private static func withOptionalCString<T>(
        _ string: String?, _ body: (UnsafePointer<CChar>?) -> T
    ) -> T {
        guard let string else { return body(nil) }
        return string.withCString(body)
    }

    /// Every file that differs between the trees of two revisions, sorted by path. `nil` for
    /// `from` stands for the empty tree, so a root commit diffs as pure additions.
    public func diffTrees(from: Oid?, to: Oid) throws -> [TreeChange] {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = withOptionalOid(from) { fromPtr in
            to.withC { toPtr in
                rcgit_diff_trees(handle, fromPtr, toPtr, &error)
            }
        }
        guard let list else {
            throw Repository.take(error, fallback: "cannot diff trees")
        }
        defer { rcgit_tree_change_list_free(list) }
        var changes: [TreeChange] = []
        changes.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            changes.append(TreeChange(c: list.pointee.changes[index]))
        }
        return changes
    }

    /// Line diff between two blobs; pass `nil` for the absent side of an added or deleted
    /// file.
    public func diffBlobs(
        old: Oid?, new: Oid?, limit: UInt64 = Repository.defaultBlobLimit, context: UInt32 = 3
    ) throws -> BlobDiff {
        var error: UnsafeMutablePointer<CChar>? = nil
        let diff = withOptionalOid(old) { oldPtr in
            withOptionalOid(new) { newPtr in
                rcgit_diff_blobs(handle, oldPtr, newPtr, limit, context, &error)
            }
        }
        guard let diff else {
            throw Repository.take(error, fallback: "cannot diff blobs")
        }
        defer { rcgit_blob_diff_free(diff) }
        return Repository.blobDiff(from: diff.pointee)
    }

    /// Line diff between the blob at `old` and the working-tree file at `path`, both as raw
    /// bytes. `nil` for `old` renders an untracked file as pure additions.
    public func diffWorktreeFile(
        path: String, old: Oid?, limit: UInt64 = Repository.defaultBlobLimit, context: UInt32 = 3
    ) throws -> BlobDiff {
        var error: UnsafeMutablePointer<CChar>? = nil
        let diff = withOptionalOid(old) { oldPtr in
            rcgit_diff_worktree_file(handle, oldPtr, path, limit, context, &error)
        }
        guard let diff else {
            throw Repository.take(error, fallback: "cannot diff '\(path)' against the tree")
        }
        defer { rcgit_blob_diff_free(diff) }
        return Repository.blobDiff(from: diff.pointee)
    }

    private static func blobDiff(from c: RcGitBlobDiff) -> BlobDiff {
        switch c.kind {
        case RcGitBlobDiffKind_Text:
            var hunks: [DiffHunk] = []
            hunks.reserveCapacity(c.hunk_count)
            for index in 0..<c.hunk_count {
                hunks.append(DiffHunk(c: c.hunks[index]))
            }
            return .text(hunks: hunks)
        case RcGitBlobDiffKind_Binary:
            return .binary(oldSize: c.old_size, newSize: c.new_size)
        default:
            return .tooLarge(size: c.size, limit: c.limit)
        }
    }

    /// Attributes every line of `path` at commit `tip` to the commit that introduced it.
    /// Walks history; call it for the file on screen, never in bulk.
    public func blame(tip: Oid, path: String) throws -> [BlameRange] {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = tip.withC { rcgit_blame(handle, $0, path, &error) }
        guard let list else {
            throw Repository.take(error, fallback: "cannot blame \(path)")
        }
        defer { rcgit_blame_list_free(list) }
        var ranges: [BlameRange] = []
        ranges.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.ranges[index]
            ranges.append(
                BlameRange(
                    commit: Oid(c: c.commit), startLine: c.start_line, lineCount: c.line_count))
        }
        return ranges
    }

    private func withOptionalOid<T>(
        _ oid: Oid?, _ body: (UnsafePointer<RcGitOid>?) throws -> T
    ) rethrows -> T {
        guard let oid else { return try body(nil) }
        return try oid.withC { try body($0) }
    }

    /// One working-tree directory with a status overlay against the tree at the same path
    /// below `tree`. `dir` is repository-relative; empty for the root.
    public func worktreeEntries(tree: Oid, dir: String) throws -> WorktreeListing {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = tree.withC { rcgit_worktree_entries(handle, $0, dir, &error) }
        guard let list else {
            throw Repository.take(error, fallback: "cannot list working tree at '\(dir)'")
        }
        defer { rcgit_worktree_list_free(list) }
        var entries: [WorktreeEntry] = []
        entries.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.entries[index]
            entries.append(
                WorktreeEntry(
                    name: String(cString: c.name),
                    kind: EntryKind(c: c.kind),
                    status: WorktreeStatus(c: c.status),
                    oid: c.has_oid ? Oid(c: c.oid) : nil,
                    size: c.size
                ))
        }
        var missing: [TreeEntry] = []
        missing.reserveCapacity(list.pointee.missing_len)
        for index in 0..<list.pointee.missing_len {
            missing.append(TreeEntry(c: list.pointee.missing[index]))
        }
        return WorktreeListing(entries: entries, missing: missing)
    }

    /// Every changed working-tree path against `tree`: modified, untracked, and deleted
    /// files, sorted by path. The walk stops at `maxScan` visited entries and reports the
    /// truncation.
    public func worktreeStatus(tree: Oid, maxScan: Int = Repository.defaultStatusScanCap)
        throws -> StatusReport
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = tree.withC { rcgit_worktree_status(handle, $0, maxScan, &error) }
        guard let list else {
            throw Repository.take(error, fallback: "cannot scan the working tree")
        }
        defer { rcgit_status_list_free(list) }
        var entries: [StatusEntry] = []
        entries.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.entries[index]
            entries.append(
                StatusEntry(
                    path: String(cString: c.path),
                    kind: EntryKind(c: c.kind),
                    status: WorktreeStatus(c: c.status),
                    oid: c.has_oid ? Oid(c: c.oid) : nil,
                    size: c.size
                ))
        }
        return StatusReport(
            entries: entries, scanned: list.pointee.scanned, truncated: list.pointee.truncated)
    }

    /// Every stash entry, newest first; empty when the repository has no stash.
    public func stashList() throws -> [StashEntry] {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = rcgit_stash_list(handle, &error) else {
            throw Repository.take(error, fallback: "cannot read the stash")
        }
        defer { rcgit_stash_list_free(list) }
        var entries: [StashEntry] = []
        entries.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.entries[index]
            entries.append(
                StashEntry(
                    index: c.index, oid: Oid(c: c.oid), message: String(cString: c.message)))
        }
        return entries
    }

    /// Reads one working-tree file for display; a symlink yields its target as text.
    public func worktreeFile(path: String, limit: UInt64 = Repository.defaultBlobLimit) throws
        -> BlobContent
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let blob = rcgit_worktree_read(handle, path, limit, &error) else {
            throw Repository.take(error, fallback: "cannot read '\(path)' from the working tree")
        }
        defer { rcgit_blob_free(blob) }
        let c = blob.pointee
        switch c.kind {
        case RcGitBlobKind_Text:
            let data = Array(UnsafeBufferPointer(start: c.data, count: c.data_len))
            let lineStarts = Array(UnsafeBufferPointer(start: c.line_starts, count: c.line_count))
            return .text(data: data, lineStarts: lineStarts, validUTF8: c.valid_utf8)
        case RcGitBlobKind_Binary:
            return .binary(size: c.size)
        default:
            return .tooLarge(size: c.size, limit: c.limit)
        }
    }

    public func commit(_ oid: Oid) throws -> CommitSummary {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let commit = oid.withC({ rcgit_commit(handle, $0, &error) }) else {
            throw Repository.take(error, fallback: "cannot read commit \(oid)")
        }
        defer { rcgit_commit_free(commit) }
        return CommitSummary(c: commit.pointee)
    }

    /// One commit with its full message, committer, and root tree.
    public func commitDetails(_ oid: Oid) throws -> CommitDetail {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let detail = oid.withC({ rcgit_commit_details(handle, $0, &error) }) else {
            throw Repository.take(error, fallback: "cannot read commit \(oid)")
        }
        defer { rcgit_commit_detail_free(detail) }
        let c = detail.pointee
        return CommitDetail(
            summary: CommitSummary(c: c.summary),
            body: String(cString: c.body),
            committerName: String(cString: c.committer_name),
            committerEmail: String(cString: c.committer_email),
            committerTime: c.committer_time,
            committerOffset: c.committer_offset,
            tree: Oid(c: c.tree)
        )
    }

    private static func take(_ error: UnsafeMutablePointer<CChar>?, fallback: String)
        -> RepositoryError
    {
        RepositoryError(takeString(error) ?? fallback)
    }

    private static func takeString(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { rcgit_string_free(pointer) }
        return String(cString: pointer)
    }
}

extension TreeEntry {
    init(c: RcGitTreeEntry) {
        self.init(name: String(cString: c.name), oid: Oid(c: c.oid), kind: EntryKind(c: c.kind))
    }
}

extension TreeChange {
    init(c: RcGitTreeChange) {
        self.init(
            kind: ChangeKind(c: c.kind),
            path: String(cString: c.path),
            sourcePath: c.source_path.map { String(cString: $0) },
            old: c.has_old ? Oid(c: c.old_id) : nil,
            new: c.has_new ? Oid(c: c.new_id) : nil,
            entry: EntryKind(c: c.entry)
        )
    }
}

extension DiffHunk {
    init(c: RcGitDiffHunk) {
        var lines: [DiffLine] = []
        lines.reserveCapacity(c.line_count)
        for index in 0..<c.line_count {
            let line = c.lines[index]
            lines.append(
                DiffLine(kind: DiffLineKind(c: line.kind), text: String(cString: line.text)))
        }
        self.init(
            oldStart: c.old_start, oldLines: c.old_lines, newStart: c.new_start,
            newLines: c.new_lines, lines: lines)
    }
}

extension CommitSummary {
    init(c: RcGitCommit) {
        var parents: [Oid] = []
        for index in 0..<c.parent_count {
            parents.append(Oid(c: c.parents[index]))
        }
        self.init(
            oid: Oid(c: c.oid),
            parents: parents,
            authorName: String(cString: c.author_name),
            authorEmail: String(cString: c.author_email),
            authorTime: c.author_time,
            authorOffset: c.author_offset,
            title: String(cString: c.title)
        )
    }
}
