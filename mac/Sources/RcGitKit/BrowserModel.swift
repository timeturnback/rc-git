import Foundation

/// What the file pane shows.
public enum FileState: Equatable, Sendable {
    case empty
    case loading(TreeEntry)
    case text(TreeEntry, TextDocument)
    case binary(TreeEntry, size: UInt64)
    case tooLarge(TreeEntry, size: UInt64, limit: UInt64)
    case failed(TreeEntry, String)
}

/// What the commit detail header shows.
public enum CommitDetailState: Equatable, Sendable {
    case empty
    case loading(CommitSummary)
    case loaded(CommitDetail, refs: [RefEntry])
    case failed(CommitSummary, String)
}

/// State for one browser window: the open repository, its refs, the selected ref's root tree,
/// the commit history of that ref, and the file being viewed. All members are main-actor;
/// work runs on background tasks and comes back through the callbacks, which fire on the
/// main actor.
///
/// Every asynchronous load carries a generation number. When the user moves on before a load
/// finishes, the stale result is dropped instead of overwriting the newer state.
@MainActor
public final class BrowserModel {
    public private(set) var repository: (any RepositoryReading)?
    public private(set) var refs = RefListing(refs: [])
    public private(set) var head: Head?
    public private(set) var selectedRef: RefEntry?
    /// Set instead of `selectedRef` while browsing the tree of one commit from the history.
    public private(set) var browsedCommit: CommitSummary?
    /// True while the tree pane shows the working directory instead of a committed tree.
    public private(set) var browsingWorkingTree = false
    public private(set) var rootTree: Oid?
    public private(set) var fileState: FileState = .empty
    /// Repository-relative path of the file in the file pane, when the opener knew it.
    public private(set) var openedFilePath: String?
    public private(set) var lastError: String?
    public let trees = TreeStore()

    /// The commit whose changed files the history detail shows.
    public private(set) var diffCommit: CommitSummary?
    /// Files changed by `diffCommit` against its first parent.
    public private(set) var commitChanges: [TreeChange] = []
    /// The line diff for one entry of `commitChanges`.
    public private(set) var fileDiff: FileDiffState = .empty
    /// Full message, committer, and refs of `diffCommit`.
    public private(set) var commitDetail: CommitDetailState = .empty
    /// Commits left behind by `jumpToCommit`, most recent last.
    public private(set) var detailBackStack: [CommitSummary] = []

    /// Commits of the selected ref loaded so far, newest first.
    public private(set) var history: [CommitSummary] = []
    /// True once a page came back shorter than `historyPageSize`.
    public private(set) var historyComplete = false
    public private(set) var isLoadingHistory = false
    /// How many commits one call to `loadMoreHistory` fetches.
    public static let historyPageSize = 100

    /// Fired after refs, HEAD, or the selected ref changed.
    public var onRefsChanged: (() -> Void)?
    /// Fired after the root tree changed; the tree view should reload from scratch.
    public var onRootChanged: (() -> Void)?
    /// Fired after `fileState` changed.
    public var onFileChanged: (() -> Void)?
    /// Fired after `history` changed.
    public var onHistoryChanged: (() -> Void)?
    /// Fired after `diffCommit`, `commitChanges`, or `fileDiff` changed.
    public var onDiffChanged: (() -> Void)?
    /// Fired when a background operation failed with a message worth showing.
    public var onError: ((String) -> Void)?

    private var rootGeneration = 0
    private var fileGeneration = 0
    private var refreshGeneration = 0
    private var historyGeneration = 0
    private var diffGeneration = 0
    /// The tip whose history the history pane shows. Kept while browsing one of its commits.
    private var historyAnchor: Oid?
    /// Pages already walked, per tip. A tip is immutable, so entries never go stale.
    private var historyCache: [Oid: (commits: [CommitSummary], complete: Bool)] = [:]
    /// Flattened file lists, per root tree. Content-addressed like every other cache.
    private var pathIndices: [Oid: PathIndex] = [:]
    private var indexBuild: (root: Oid, task: Task<PathIndex, any Error>)?
    /// Syntax spans, per blob. Content-addressed like every other cache.
    private var highlightCache: [Oid: [HighlightSpan]] = [:]

    public init() {}

    /// Attaches a repository and loads refs and HEAD, then selects the branch HEAD is on.
    public func attach(_ repository: any RepositoryReading) async {
        self.repository = repository
        trees.removeAll()
        historyCache.removeAll()
        pathIndices.removeAll()
        highlightCache.removeAll()
        indexBuild?.task.cancel()
        indexBuild = nil
        browsedCommit = nil
        browsingWorkingTree = false
        resetHistory(anchor: nil)
        fileState = .empty
        openedFilePath = nil
        onFileChanged?()
        rootGeneration += 1
        refreshGeneration += 1
        let generation = rootGeneration
        let loaded = await Task.detached(priority: .userInitiated) {
            Self.loadListing(from: repository)
        }.value
        guard generation == rootGeneration else { return }
        switch loaded {
        case let .success((listing, head)):
            refs = listing
            self.head = head
            lastError = nil
            onRefsChanged?()
            if let current = initialRef(listing: listing, head: head) {
                await select(current)
            }
        case let .failure(error):
            fail(error.message)
        }
    }

    /// Reloads refs and HEAD after the repository changed on disk, keeping every object
    /// cache: only refs can move. The selection survives by name; when its target moved the
    /// root tree and history reload, and when it disappeared the selection falls back the
    /// same way `attach` chooses one. A failed refresh keeps the last good listing and does
    /// not alert: the watcher fires during ref updates, when transient lock errors are normal.
    public func refreshRefs() async {
        guard let repository else { return }
        refreshGeneration += 1
        let generation = refreshGeneration
        let loaded = await Task.detached(priority: .userInitiated) {
            Self.loadListing(from: repository)
        }.value
        guard generation == refreshGeneration else { return }
        switch loaded {
        case let .success((listing, head)):
            let previousHead = self.head?.target
            refs = listing
            self.head = head
            lastError = nil
            if browsingWorkingTree {
                if head.target != previousHead {
                    await selectWorkingTree()
                } else {
                    onRefsChanged?()
                }
                return
            }
            guard let current = selectedRef else {
                onRefsChanged?()
                return
            }
            if let updated = listing.refs.first(where: { $0.fullName == current.fullName }) {
                if updated.target == current.target {
                    selectedRef = updated
                    onRefsChanged?()
                } else {
                    await select(updated)
                }
            } else if let fallback = initialRef(listing: listing, head: head) {
                await select(fallback)
            } else {
                selectedRef = nil
                rootTree = nil
                resetHistory(anchor: nil)
                onRefsChanged?()
                onRootChanged?()
            }
        case let .failure(error):
            lastError = error.message
        }
    }

    /// The branch `HEAD` is on, else the first local branch, else the first ref of any kind.
    public func initialRef(listing: RefListing, head: Head) -> RefEntry? {
        if let name = head.refName, let entry = listing.refs.first(where: { $0.fullName == name }) {
            return entry
        }
        if let target = head.target, let entry = listing.refs.first(where: { $0.target == target })
        {
            return entry
        }
        return listing.refs.first(where: { $0.kind == .localBranch }) ?? listing.refs.first
    }

    /// Switches the browsed ref. Only the root tree id changes; cached subtrees stay valid.
    public func select(_ ref: RefEntry) async {
        guard repository != nil else { return }
        selectedRef = ref
        browsedCommit = nil
        browsingWorkingTree = false
        onRefsChanged?()
        resetHistory(anchor: ref.target)
        await loadRoot(of: ref.target)
    }

    /// Shows the tree of one commit, picked from the history pane. The sidebar deselects and
    /// the history keeps showing the ref the commit came from, so hopping between commits
    /// stays one click.
    public func browseCommit(_ commit: CommitSummary) async {
        guard repository != nil else { return }
        selectedRef = nil
        browsedCommit = commit
        browsingWorkingTree = false
        onRefsChanged?()
        await loadRoot(of: commit.oid)
    }

    /// Shows the files on disk with a status overlay against `HEAD`. Reads only; nothing in
    /// rc-git ever writes to the working tree.
    public func selectWorkingTree() async {
        guard repository != nil, let target = head?.target else { return }
        selectedRef = nil
        browsedCommit = nil
        browsingWorkingTree = true
        onRefsChanged?()
        resetHistory(anchor: target)
        await loadRoot(of: target)
    }

    /// One working-tree directory as display rows: disk entries with their status, then the
    /// files the compared tree has but disk lost, in one display order. Not cached: disk is
    /// the one thing content addressing cannot cover.
    public func worktreeRows(at dir: String) async throws -> [BrowseRow] {
        guard let repository else { throw RepositoryError("no repository is open") }
        guard let root = rootTree else { return [] }
        return try await Task.detached(priority: .userInitiated) {
            let listing = try repository.worktreeEntries(tree: root, dir: dir)
            return Self.rows(for: listing)
        }.value
    }

    /// Merges a listing into display order: directories first, then names, deleted files in
    /// their sorted place. Pure, so the ordering is testable.
    nonisolated public static func rows(for listing: WorktreeListing) -> [BrowseRow] {
        var rows: [BrowseRow] = []
        rows.reserveCapacity(listing.entries.count + listing.missing.count)
        for entry in listing.entries {
            rows.append(
                BrowseRow(
                    entry: TreeEntry(
                        name: entry.name, oid: entry.oid ?? Oid(bytes: []), kind: entry.kind),
                    status: entry.status,
                    missing: false
                ))
        }
        for entry in listing.missing {
            rows.append(BrowseRow(entry: entry, status: nil, missing: true))
        }
        rows.sort {
            let leftIsDir = $0.entry.kind.isDirectory
            let rightIsDir = $1.entry.kind.isDirectory
            if leftIsDir != rightIsDir { return leftIsDir }
            return $0.entry.name.lowercased() < $1.entry.name.lowercased()
        }
        return rows
    }

    /// Opens a file straight from the working directory into the file pane.
    public func openWorktreeFile(
        name: String, path: String, limit: UInt64 = Repository.defaultBlobLimit
    ) async {
        guard let repository else { return }
        let entry = TreeEntry(name: name, oid: Oid(bytes: []), kind: .blob)
        fileGeneration += 1
        let generation = fileGeneration
        openedFilePath = path
        fileState = .loading(entry)
        onFileChanged?()
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<BlobContent, RepositoryError> in
            do { return .success(try repository.worktreeFile(path: path, limit: limit)) } catch {
                return .failure(Self.wrap(error))
            }
        }.value
        guard generation == fileGeneration else { return }
        switch result {
        case let .success(content):
            fileState = Self.state(for: entry, content: content)
        case let .failure(error):
            fileState = .failed(entry, error.message)
        }
        onFileChanged?()
    }

    private func loadRoot(of target: Oid) async {
        guard let repository else { return }
        rootGeneration += 1
        let generation = rootGeneration
        indexBuild?.task.cancel()
        indexBuild = nil
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<Oid, RepositoryError> in
            do { return .success(try repository.treeOf(target)) } catch {
                return .failure(Self.wrap(error))
            }
        }.value
        guard generation == rootGeneration else { return }
        switch result {
        case let .success(tree):
            rootTree = tree
            fileState = .empty
            openedFilePath = nil
            onFileChanged?()
            onRootChanged?()
        case let .failure(error):
            fail(error.message)
        }
    }

    /// Children of a directory, from the cache when possible. Safe to call for any tree id,
    /// including one from a ref that is no longer selected.
    public func children(of tree: Oid) async throws -> [TreeEntry] {
        if let cached = trees.cached(tree) { return cached }
        guard let repository else { throw RepositoryError("no repository is open") }
        let store = trees
        return try await Task.detached(priority: .userInitiated) {
            let entries = try repository.treeEntries(tree)
            store.store(entries, for: tree)
            return entries
        }.value
    }

    /// Loads a file into the file pane. `limit` caps how much is decoded; pass a larger value
    /// to load a file the default limit rejected. `path` is the repository-relative location,
    /// kept for blame; pass `nil` when it is unknown.
    public func openFile(
        _ entry: TreeEntry, path: String? = nil, limit: UInt64 = Repository.defaultBlobLimit
    ) async {
        guard let repository, !entry.kind.isDirectory else { return }
        fileGeneration += 1
        let generation = fileGeneration
        if let path { openedFilePath = path }
        fileState = .loading(entry)
        onFileChanged?()
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<BlobContent, RepositoryError> in
            do { return .success(try repository.readBlob(entry.oid, limit: limit)) } catch {
                return .failure(Self.wrap(error))
            }
        }.value
        guard generation == fileGeneration else { return }
        switch result {
        case let .success(content):
            fileState = Self.state(for: entry, content: content)
        case let .failure(error):
            fileState = .failed(entry, error.message)
        }
        onFileChanged?()
    }

    // MARK: History

    /// Fetches the next page of commits for the history pane. Does nothing while a page is
    /// loading, after the last page, or when nothing is selected. Pages accumulate in a
    /// cache keyed by the tip, so switching away and back does not re-walk.
    public func loadMoreHistory() async {
        guard let repository, let tip = historyAnchor, !isLoadingHistory, !historyComplete
        else { return }
        isLoadingHistory = true
        let generation = historyGeneration
        let skip = history.count
        let pageSize = Self.historyPageSize
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<[CommitSummary], RepositoryError> in
            do { return .success(try repository.log(tip: tip, skip: skip, limit: pageSize)) } catch
            {
                return .failure(Self.wrap(error))
            }
        }.value
        guard generation == historyGeneration else { return }
        isLoadingHistory = false
        switch result {
        case let .success(page):
            history += page
            historyComplete = page.count < pageSize
            historyCache[tip] = (history, historyComplete)
            onHistoryChanged?()
        case let .failure(error):
            fail(error.message)
        }
    }

    private func resetHistory(anchor: Oid?) {
        historyGeneration += 1
        isLoadingHistory = false
        historyAnchor = anchor
        diffGeneration += 1
        detailBackStack = []
        if diffCommit != nil {
            diffCommit = nil
            commitChanges = []
            fileDiff = .empty
            commitDetail = .empty
            onDiffChanged?()
        }
        if let anchor, let cached = historyCache[anchor] {
            history = cached.commits
            historyComplete = cached.complete
        } else {
            history = []
            historyComplete = false
        }
        onHistoryChanged?()
    }

    // MARK: Diff and blame

    /// Loads the files changed by `commit` against its first parent, and the commit's full
    /// detail, for the history detail. A root commit diffs against the empty tree.
    public func loadCommitChanges(_ commit: CommitSummary) async {
        guard let repository else { return }
        diffGeneration += 1
        let generation = diffGeneration
        diffCommit = commit
        commitChanges = []
        fileDiff = .empty
        commitDetail = .loading(commit)
        onDiffChanged?()
        let parent = commit.parents.first
        let target = commit.oid
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<([TreeChange], CommitDetail), RepositoryError> in
            do {
                let changes = try repository.diffTrees(from: parent, to: target)
                let detail = try repository.commitDetails(target)
                return .success((changes, detail))
            } catch {
                return .failure(Self.wrap(error))
            }
        }.value
        guard generation == diffGeneration else { return }
        switch result {
        case let .success((changes, detail)):
            commitChanges = changes
            commitDetail = .loaded(detail, refs: refsPointing(at: target))
            if let first = changes.first {
                await loadFileDiff(first)
            } else {
                onDiffChanged?()
            }
        case let .failure(error):
            commitDetail = .failed(commit, error.message)
            fail(error.message)
        }
    }

    /// Every ref whose target is `oid`, for the detail header's badges.
    public func refsPointing(at oid: Oid) -> [RefEntry] {
        refs.refs.filter { $0.target == oid }
    }

    /// Shows the detail of a commit reached by id — a parent link or a revision jump. The
    /// current detail commit is pushed for `goBackInDetail`.
    public func jumpToCommit(_ oid: Oid) async {
        guard let repository else { return }
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<CommitSummary, RepositoryError> in
            do { return .success(try repository.commit(oid)) } catch {
                return .failure(Self.wrap(error))
            }
        }.value
        switch result {
        case let .success(summary):
            if let current = diffCommit, current.oid != oid {
                detailBackStack.append(current)
            }
            await loadCommitChanges(summary)
        case let .failure(error):
            fail(error.message)
        }
    }

    /// Returns to the commit left behind by the last `jumpToCommit`.
    public func goBackInDetail() async {
        guard let previous = detailBackStack.popLast() else { return }
        await loadCommitChanges(previous)
    }

    /// Loads the line diff of one changed file from `commitChanges`.
    public func loadFileDiff(_ change: TreeChange) async {
        guard let repository else { return }
        diffGeneration += 1
        let generation = diffGeneration
        fileDiff = .loading(change)
        onDiffChanged?()
        let result = await Task.detached(priority: .userInitiated) {
            () -> Result<BlobDiff, RepositoryError> in
            do {
                return .success(
                    try repository.diffBlobs(
                        old: change.old, new: change.new,
                        limit: Repository.defaultBlobLimit, context: 3))
            } catch {
                return .failure(Self.wrap(error))
            }
        }.value
        guard generation == diffGeneration else { return }
        switch result {
        case let .success(diff):
            fileDiff = .loaded(change, diff)
        case let .failure(error):
            fileDiff = .failed(change, error.message)
        }
        onDiffChanged?()
    }

    /// Blames the file in the file pane at the browsed revision. Returns the ranges paired
    /// with their commit summaries, resolved through a cache so each commit is read once.
    ///
    /// # Errors
    /// `RepositoryError` when no file with a known path is open or the walk fails.
    public func blameOpenedFile() async throws -> [BlameSpan] {
        guard let repository else { throw RepositoryError("no repository is open") }
        guard let path = openedFilePath else {
            throw RepositoryError("no file with a known path is open")
        }
        guard let tip = selectedRef?.target ?? browsedCommit?.oid else {
            throw RepositoryError("nothing is selected to blame at")
        }
        return try await Task.detached(priority: .userInitiated) {
            let ranges = try repository.blame(tip: tip, path: path)
            var commits: [Oid: CommitSummary] = [:]
            var spans: [BlameSpan] = []
            for range in ranges {
                let commit: CommitSummary
                if let cached = commits[range.commit] {
                    commit = cached
                } else {
                    commit = try repository.commit(range.commit)
                    commits[range.commit] = commit
                }
                spans.append(BlameSpan(range: range, commit: commit))
            }
            return spans
        }.value
    }

    // MARK: Highlighting

    /// Syntax spans for a text file, computed in the background and cached by blob id, so a
    /// file parses once however often it is reopened. `nil` for unknown languages, files
    /// whose bytes did not decode cleanly (offsets would misalign), or oversized files.
    public func highlights(for entry: TreeEntry, document: TextDocument) async
        -> [HighlightSpan]?
    {
        guard !document.isLossy, document.byteCount <= Highlighter.byteLimit else { return nil }
        let cacheable = !entry.oid.bytes.isEmpty
        if cacheable, let cached = highlightCache[entry.oid] { return cached }
        let name = entry.name
        let text = document.text
        let spans = await Task.detached(priority: .utility) {
            Highlighter.highlight(name: name, data: Array(text.utf8))
        }.value
        if cacheable, let spans {
            highlightCache[entry.oid] = spans
        }
        return spans
    }

    // MARK: Path search

    /// The flattened file list below `root`, building it in the background on first use.
    /// This is the one call that walks a whole tree; it checks for cancellation between
    /// directory reads and every directory it reads lands in the shared tree cache.
    public func pathIndex(for root: Oid) async throws -> PathIndex {
        if let cached = pathIndices[root] { return cached }
        if let build = indexBuild, build.root == root { return try await build.task.value }
        guard let repository else { throw RepositoryError("no repository is open") }
        indexBuild?.task.cancel()
        let store = trees
        let task = Task.detached(priority: .utility) {
            try PathIndex.build(root: root) { tree in
                if let cached = store.cached(tree) { return cached }
                let entries = try repository.treeEntries(tree)
                store.store(entries, for: tree)
                return entries
            }
        }
        indexBuild = (root, task)
        defer {
            if indexBuild?.root == root { indexBuild = nil }
        }
        let index = try await task.value
        pathIndices[root] = index
        return index
    }

    /// The best matches for `query` among every file below the current root. Builds the
    /// index on first use; a query while a build for another root is running cancels it.
    public func searchPaths(_ query: String, limit: Int = 100) async throws -> [PathIndexEntry] {
        guard let root = rootTree else { return [] }
        let index = try await pathIndex(for: root)
        return await Task.detached(priority: .userInitiated) {
            index.search(query, limit: limit)
        }.value
    }

    // MARK: Helpers

    /// Maps blob content to a pane state. Pure, so the mapping is testable.
    public static func state(for entry: TreeEntry, content: BlobContent) -> FileState {
        switch content {
        case let .text(data, lineStarts):
            return .text(entry, TextDocument(bytes: data, lineStarts: lineStarts))
        case let .binary(size):
            return .binary(entry, size: size)
        case let .tooLarge(size, limit):
            return .tooLarge(entry, size: size, limit: limit)
        }
    }

    nonisolated private static func loadListing(from repository: any RepositoryReading)
        -> Result<(RefListing, Head), RepositoryError>
    {
        do {
            return .success((try repository.refs(), try repository.head()))
        } catch {
            return .failure(Self.wrap(error))
        }
    }

    private func fail(_ message: String) {
        lastError = message
        onError?(message)
    }

    nonisolated private static func wrap(_ error: any Error) -> RepositoryError {
        (error as? RepositoryError) ?? RepositoryError(String(describing: error))
    }
}
