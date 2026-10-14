import CRcGit
import Foundation

/// An open repository, backed by the Rust core. Safe to use from any thread: the core keeps a
/// pool of handles and never blocks one call on another.
public final class Repository: RepositoryReading, @unchecked Sendable {
    /// Default cap for `readBlob`: 8 MiB, matching the core.
    public static let defaultBlobLimit: UInt64 = 8 * 1024 * 1024

    /// Default context lines around each diff hunk: 3, matching the core's
    /// `DEFAULT_DIFF_CONTEXT`. The guard recomputes hunk stamps with the core constant, so a
    /// stamp taken at any other context fails as a hunk mismatch.
    public static let defaultDiffContext: UInt32 = 3

    /// Default cap for `worktreeStatus`, matching the path-search precedent.
    public static let defaultStatusScanCap = 200_000

    fileprivate let handle: OpaquePointer

    /// Opens a working directory or a `.git` directory.
    public init(path: String) throws {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let handle = rcgit_repo_open(path, &error) else {
            throw RepositoryError.take(error, fallback: "cannot open \(path)")
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

    /// This worktree's git directory: `.git`, or `.git/worktrees/<name>` for a linked worktree.
    public var gitDirectory: String {
        RepositoryError.takeString(rcgit_repo_git_dir(handle)) ?? ""
    }

    /// The git directory every worktree shares, where branches, tags and `packed-refs` live.
    /// Equal to `gitDirectory` except in a linked worktree.
    public var commonDirectory: String {
        RepositoryError.takeString(rcgit_repo_common_dir(handle)) ?? ""
    }

    /// The working directory, or `nil` for a bare repository.
    public var workingDirectory: String? {
        RepositoryError.takeString(rcgit_repo_work_dir(handle))
    }

    public func head() throws -> Head {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let head = rcgit_head(handle, &error) else {
            throw RepositoryError.take(error, fallback: "cannot read HEAD")
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
                throw RepositoryError.take(error, fallback: "cannot read the default branch")
            }
            return nil
        }
        return RepositoryError.takeString(branch)
    }

    public func refs() throws -> RefListing {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = rcgit_refs(handle, &error) else {
            throw RepositoryError.take(error, fallback: "cannot list references")
        }
        defer { rcgit_ref_list_free(list) }
        var refs: [RefEntry] = []
        refs.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.refs[index]
            let fullName = ExactName(c.full_name)
            let shortName = ExactName(c.short_name)
            refs.append(
                RefEntry(
                    fullName: fullName.text,
                    shortName: shortName.text,
                    kind: RefKind(c: c.kind),
                    target: Oid(c: c.target),
                    hasExactName: c.has_exact_name && fullName.exact && shortName.exact
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
            throw RepositoryError.take(error, fallback: "cannot resolve \(spec)")
        }
        return Oid(c: out)
    }

    public func resolveTree(_ spec: String) throws -> Oid {
        var out = RcGitOid()
        var error: UnsafeMutablePointer<CChar>? = nil
        guard rcgit_resolve_tree(handle, spec, &out, &error) == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot resolve \(spec)")
        }
        return Oid(c: out)
    }

    public func treeOf(_ oid: Oid) throws -> Oid {
        var out = RcGitOid()
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = oid.withC { rcgit_tree_of(handle, $0, &out, &error) }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot peel \(oid)")
        }
        return Oid(c: out)
    }

    public func treeEntries(_ tree: Oid) throws -> [TreeEntry] {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = tree.withC({ rcgit_tree_entries(handle, $0, &error) }) else {
            throw RepositoryError.take(error, fallback: "cannot read tree \(tree)")
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
            if let error { throw RepositoryError.take(error, fallback: "cannot look up \(path)") }
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
            throw RepositoryError.take(error, fallback: "cannot read blob \(oid)")
        }
        defer { rcgit_blob_free(blob) }
        return BlobContent(c: blob.pointee)
    }

    /// Size in bytes, from the object header, without decoding the content.
    public func blobSize(_ oid: Oid) throws -> UInt64 {
        var size: UInt64 = 0
        var error: UnsafeMutablePointer<CChar>? = nil
        let status = oid.withC { rcgit_blob_size(handle, $0, &size, &error) }
        guard status == RCGIT_OK else {
            throw RepositoryError.take(error, fallback: "cannot size blob \(oid)")
        }
        return size
    }

    /// A cursor over the commits reachable from `tip`, newest first. Each `next` resumes
    /// where the previous one stopped.
    public func logCursor(tip: Oid) throws -> any LogCursorReading {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let cursor = tip.withC({ rcgit_log_cursor_new(handle, $0, &error) }) else {
            throw RepositoryError.take(error, fallback: "cannot walk history from \(tip)")
        }
        return LogCursor(repository: self, handle: cursor)
    }

    /// A walk over the commits reachable from `tip` matching every set field of `filter`,
    /// newest first; a filter with only `path` set is that path's history. Each `next`
    /// resumes where the previous one stopped. A set `after` also ends the walk at that time.
    public func historyWalk(tip: Oid, filter: CommitFilter) throws -> any HistoryWalkReading {
        var error: UnsafeMutablePointer<CChar>? = nil
        let walk = tip.withC { tipPtr in
            Repository.withOptionalCString(filter.message) { messagePtr in
                Repository.withOptionalCString(filter.author) { authorPtr in
                    Repository.withOptionalCString(filter.path) { pathPtr in
                        let cFilter = RcGitCommitFilter(
                            message: messagePtr, author: authorPtr,
                            has_after: filter.after != nil, after: filter.after ?? 0,
                            has_before: filter.before != nil, before: filter.before ?? 0,
                            path: pathPtr)
                        return rcgit_history_walk_new(handle, tipPtr, cFilter, &error)
                    }
                }
            }
        }
        guard let walk else {
            throw RepositoryError.take(error, fallback: "cannot search history from \(tip)")
        }
        return HistoryWalk(repository: self, handle: walk)
    }

    fileprivate static func commitList(_ c: RcGitCommitList) -> [CommitSummary] {
        var commits: [CommitSummary] = []
        commits.reserveCapacity(c.len)
        for index in 0..<c.len {
            commits.append(CommitSummary(c: c.commits[index]))
        }
        return commits
    }

    fileprivate static func commitPage(_ c: RcGitCommitPage) -> CommitPage {
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
            throw RepositoryError.take(error, fallback: "cannot diff trees")
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
        old: Oid?, new: Oid?, limit: UInt64 = Repository.defaultBlobLimit,
        context: UInt32 = Repository.defaultDiffContext
    ) throws -> BlobDiff {
        var error: UnsafeMutablePointer<CChar>? = nil
        let diff = withOptionalOid(old) { oldPtr in
            withOptionalOid(new) { newPtr in
                rcgit_diff_blobs(handle, oldPtr, newPtr, limit, context, &error)
            }
        }
        guard let diff else {
            throw RepositoryError.take(error, fallback: "cannot diff blobs")
        }
        defer { rcgit_blob_diff_free(diff) }
        return Repository.blobDiff(from: diff.pointee)
    }

    /// Line diff between the blob at `old` and the working-tree file at `path`, both as raw
    /// bytes. `nil` for `old` renders an untracked file as pure additions.
    public func diffWorktreeFile(
        path: String, old: Oid?, limit: UInt64 = Repository.defaultBlobLimit,
        context: UInt32 = Repository.defaultDiffContext
    ) throws -> BlobDiff {
        var error: UnsafeMutablePointer<CChar>? = nil
        let diff = withOptionalOid(old) { oldPtr in
            rcgit_diff_worktree_file(handle, oldPtr, path, limit, context, &error)
        }
        guard let diff else {
            throw RepositoryError.take(error, fallback: "cannot diff '\(path)' against the tree")
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
    /// Walks history; call it for the file on screen, never in bulk. A file at `tip` over
    /// `limit` bytes is `.tooLarge` after one header read. `since` (seconds since the Unix
    /// epoch) stops the walk at commits older than it; lines still untraced there come back as
    /// `boundary` ranges. `nil` walks the whole ancestry. The walk cannot be cancelled.
    public func blame(
        tip: Oid, path: String, limit: UInt64 = Repository.defaultBlobLimit, since: Int64? = nil
    ) throws -> Blame {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = tip.withC { rcgit_blame(handle, $0, path, limit, since ?? Int64.min, &error) }
        guard let list else {
            throw RepositoryError.take(error, fallback: "cannot blame \(path)")
        }
        defer { rcgit_blame_list_free(list) }
        if list.pointee.kind == RcGitBlameKind_TooLarge {
            return .tooLarge(size: list.pointee.size, limit: list.pointee.limit)
        }
        var ranges: [BlameRange] = []
        ranges.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.ranges[index]
            ranges.append(
                BlameRange(
                    commit: Oid(c: c.commit), startLine: c.start_line, lineCount: c.line_count,
                    boundary: c.boundary))
        }
        return .ranges(ranges)
    }

    /// One working-tree directory with a status overlay against the tree at the same path
    /// below `tree`. `dir` is repository-relative; empty for the root.
    public func worktreeEntries(tree: Oid, dir: String) throws -> WorktreeListing {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = tree.withC { rcgit_worktree_entries(handle, $0, dir, &error) }
        guard let list else {
            throw RepositoryError.take(error, fallback: "cannot list working tree at '\(dir)'")
        }
        defer { rcgit_worktree_list_free(list) }
        var entries: [WorktreeEntry] = []
        entries.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.entries[index]
            let name = ExactName(c.name)
            entries.append(
                WorktreeEntry(
                    name: name.text,
                    kind: EntryKind(c: c.kind),
                    status: WorktreeStatus(c: c.status),
                    oid: c.has_oid ? Oid(c: c.oid) : nil,
                    size: c.size,
                    hasExactName: name.exact
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
    /// files, sorted by path. A `nil` tree (an unborn branch) lists every file as untracked.
    /// The walk stops at `maxScan` visited entries and reports the truncation.
    public func worktreeStatus(tree: Oid?, maxScan: Int = Repository.defaultStatusScanCap)
        throws -> StatusReport
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = withOptionalOid(tree) { rcgit_worktree_status(handle, $0, maxScan, &error) }
        guard let list else {
            throw RepositoryError.take(error, fallback: "cannot scan the working tree")
        }
        defer { rcgit_status_list_free(list) }
        var entries: [StatusEntry] = []
        entries.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.entries[index]
            let path = ExactName(c.path)
            entries.append(
                StatusEntry(
                    path: path.text,
                    kind: EntryKind(c: c.kind),
                    status: WorktreeStatus(c: c.status),
                    oid: c.has_oid ? Oid(c: c.oid) : nil,
                    size: c.size,
                    hasExactPath: path.exact
                ))
        }
        return StatusReport(
            entries: entries, scanned: list.pointee.scanned, truncated: list.pointee.truncated)
    }

    /// Every file on disk, sorted by path: tracked, untracked, and ignored alike. An ignored
    /// folder `tree` does not track is skipped unread. The walk stops at `maxScan` visited
    /// entries and reports the truncation.
    public func worktreeFiles(tree: Oid, maxScan: Int = PathIndex.maxEntries) throws
        -> WorktreeFileList
    {
        var error: UnsafeMutablePointer<CChar>? = nil
        let list = tree.withC { rcgit_worktree_files(handle, $0, maxScan, &error) }
        guard let list else {
            throw RepositoryError.take(error, fallback: "cannot list the working tree's files")
        }
        defer { rcgit_worktree_file_list_free(list) }
        var files: [WorktreeFile] = []
        files.reserveCapacity(list.pointee.len)
        for index in 0..<list.pointee.len {
            let c = list.pointee.files[index]
            let path = ExactName(c.path)
            files.append(
                WorktreeFile(path: path.text, kind: EntryKind(c: c.kind), hasExactPath: path.exact))
        }
        return WorktreeFileList(
            files: files, scanned: list.pointee.scanned, truncated: list.pointee.truncated)
    }

    /// Every stash entry, newest first; empty when the repository has no stash.
    public func stashList() throws -> [StashEntry] {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = rcgit_stash_list(handle, &error) else {
            throw RepositoryError.take(error, fallback: "cannot read the stash")
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
            throw RepositoryError.take(
                error, fallback: "cannot read '\(path)' from the working tree")
        }
        defer { rcgit_blob_free(blob) }
        return BlobContent(c: blob.pointee)
    }

    public func commit(_ oid: Oid) throws -> CommitSummary {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let commit = oid.withC({ rcgit_commit(handle, $0, &error) }) else {
            throw RepositoryError.take(error, fallback: "cannot read commit \(oid)")
        }
        defer { rcgit_commit_free(commit) }
        return CommitSummary(c: commit.pointee)
    }

    /// One commit with its full message, committer, and root tree.
    public func commitDetails(_ oid: Oid) throws -> CommitDetail {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let detail = oid.withC({ rcgit_commit_details(handle, $0, &error) }) else {
            throw RepositoryError.take(error, fallback: "cannot read commit \(oid)")
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
}

extension BlobContent {
    init(c: RcGitBlob) {
        switch c.kind {
        case RcGitBlobKind_Text:
            let data = Array(UnsafeBufferPointer(start: c.data, count: c.data_len))
            let lineStarts = Array(UnsafeBufferPointer(start: c.line_starts, count: c.line_count))
            self = .text(data: data, lineStarts: lineStarts, validUTF8: c.valid_utf8)
        case RcGitBlobKind_Binary:
            self = .binary(size: c.size)
        default:
            self = .tooLarge(size: c.size, limit: c.limit)
        }
    }
}

extension TreeEntry {
    init(c: RcGitTreeEntry) {
        let name = ExactName(c.name)
        self.init(
            name: name.text, oid: Oid(c: c.oid), kind: EntryKind(c: c.kind),
            hasExactName: name.exact)
    }
}

/// A name the core sent, decoded for display, and whether its bytes were valid UTF-8. Only an
/// exact name may go back to the core: the core reads path and ref arguments as UTF-8, so a
/// name repaired with U+FFFD would act on whatever carries the repaired spelling.
struct ExactName {
    let text: String
    let exact: Bool

    init(_ pointer: UnsafePointer<CChar>) {
        if let text = String(validatingCString: pointer) {
            self.text = text
            exact = true
        } else {
            text = String(cString: pointer)
            exact = false
        }
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
        let data = UnsafeBufferPointer(start: c.data, count: c.data_len)
        var lines: [DiffLine] = []
        lines.reserveCapacity(c.line_count)
        for index in 0..<c.line_count {
            let line = c.lines[index]
            let next = index + 1 < c.line_count ? Int(c.lines[index + 1].start) : data.count
            let end = min(next, data.count)
            let bytes = UnsafeBufferPointer(rebasing: data[min(Int(line.start), end)..<end])
            lines.append(
                DiffLine(
                    kind: DiffLineKind(c: line.kind), text: String(decoding: bytes, as: UTF8.self),
                    eol: LineEnd(c: line.eol)))
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

/// A log cursor of the Rust core. It keeps its repository alive, and may be used from any
/// thread because the core takes its pages one at a time behind a lock.
final class LogCursor: LogCursorReading, @unchecked Sendable {
    private let repository: Repository
    private let handle: OpaquePointer

    fileprivate init(repository: Repository, handle: OpaquePointer) {
        self.repository = repository
        self.handle = handle
    }

    deinit {
        rcgit_log_cursor_free(handle)
    }

    func next(limit: Int) throws -> [CommitSummary] {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard let list = rcgit_log_cursor_next(repository.handle, handle, limit, &error) else {
            throw RepositoryError.take(error, fallback: "cannot walk history")
        }
        defer { rcgit_commit_list_free(list) }
        return Repository.commitList(list.pointee)
    }
}

/// A filtered history walk of the Rust core. It keeps its repository alive, and may be used
/// from any thread because the core takes its pages one at a time behind a lock.
final class HistoryWalk: HistoryWalkReading, @unchecked Sendable {
    private let repository: Repository
    private let handle: OpaquePointer

    fileprivate init(repository: Repository, handle: OpaquePointer) {
        self.repository = repository
        self.handle = handle
    }

    deinit {
        rcgit_history_walk_free(handle)
    }

    func next(limit: Int, maxScan: Int) throws -> CommitPage {
        var error: UnsafeMutablePointer<CChar>? = nil
        guard
            let page = rcgit_history_walk_next(
                repository.handle, handle, limit, maxScan, &error)
        else {
            throw RepositoryError.take(error, fallback: "cannot walk history")
        }
        defer { rcgit_commit_page_free(page) }
        return Repository.commitPage(page.pointee)
    }
}
