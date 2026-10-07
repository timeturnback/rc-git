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

/// What the history pane lists: everything reachable from the anchor, or only the commits
/// that changed one path.
public enum HistoryScope: Equatable, Sendable {
    case all
    case path(String)
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
    /// Whether the file lists show entries `HiddenFiles` names. The window applies the
    /// app-wide preference; the model decides what that hides.
    public private(set) var showsHiddenFiles = false
    public let trees = TreeStore()

    /// The commit whose changed files the history detail shows.
    public private(set) var diffCommit: CommitSummary?
    /// Files changed by `diffCommit` against its first parent.
    public private(set) var commitChanges: [TreeChange] = []
    /// The line diff for one entry of `commitChanges`.
    public private(set) var fileDiff: FileDiffState = .empty
    /// Full message, committer, and refs of `diffCommit`.
    public private(set) var commitDetail: CommitDetailState = .empty

    /// Commits of the selected ref loaded so far, newest first.
    public private(set) var history: [CommitSummary] = []
    /// True once a page came back shorter than `historyPageSize`, or a filtered walk ended.
    public private(set) var historyComplete = false
    public private(set) var isLoadingHistory = false
    /// Whether the history pane shows everything or one path's commits.
    public private(set) var historyScope: HistoryScope = .all
    /// How many commits one call to `loadMoreHistory` fetches.
    public static let historyPageSize = 100
    /// How many commits one repository call may examine while filtering by path.
    public static let pathHistoryScanChunk = 5_000
    /// How many commits one repository call may examine while searching.
    public static let searchScanChunk = 10_000

    /// The active search, or `nil` while the pane shows the plain history.
    public private(set) var searchFilter: CommitFilter?
    /// Matches found so far, newest first.
    public private(set) var searchResults: [CommitSummary] = []
    /// How many commits the search has examined, for progress display.
    public private(set) var searchScanned = 0
    /// True when the search walked to the end of history.
    public private(set) var searchComplete = false
    public private(set) var isSearching = false

    /// Fired after refs, HEAD, or the selected ref changed.
    public var onRefsChanged: (() -> Void)?
    /// Fired after the root tree changed; the tree view should reload from scratch.
    public var onRootChanged: (() -> Void)?
    /// Fired after `fileState` changed.
    public var onFileChanged: (() -> Void)?
    /// Fired after `history` changed.
    public var onHistoryChanged: (() -> Void)?
    /// Fired after the search filter, results, or progress changed.
    public var onSearchChanged: (() -> Void)?
    /// Fired after `diffCommit`, `commitChanges`, or `fileDiff` changed.
    public var onDiffChanged: (() -> Void)?
    /// Fired when a background operation failed with a message worth showing.
    public var onError: ((String) -> Void)?
    /// Fired when something the user typed named nothing in the repository: a title for what
    /// was wrong, then the detail. Not a read failure, so it never sets `lastError`.
    public var onInputError: ((_ title: String, _ message: String) -> Void)?
    /// Fired after `location` changed, once per move: a ref switch that also closes the open
    /// file reports where it ended up, never the step in between.
    public var onLocationChanged: (() -> Void)?

    private var rootGeneration = 0
    private var fileGeneration = 0
    private var refreshGeneration = 0
    private var historyGeneration = 0
    private var diffGeneration = 0
    private var searchGeneration = 0
    /// A restore is running, and the location the next one goes to. A restore asked for
    /// meanwhile replaces `pendingRestore` and returns; the running one picks it up.
    private var isRestoring = false
    private var pendingRestore: BrowserLocation?
    /// How many moves are under way. `location` is half changed while one is.
    private var moveDepth = 0
    /// The location `onLocationChanged` last fired for.
    private var reportedLocation = BrowserLocation()
    /// The tip whose history the history pane shows. Kept while browsing one of its commits.
    private var historyAnchor: Oid?
    /// The unfiltered pages of one tip plus the lane graph built alongside them.
    private struct HistoryPages {
        var commits: [CommitSummary]
        var complete: Bool
        var graph: GraphBuilder
    }
    /// Pages already walked, per tip. A tip is immutable, so entries never go stale.
    private var historyCache: [Oid: HistoryPages] = [:]
    /// The lane graph of the unfiltered history, extended as pages arrive.
    private var graph = GraphBuilder()
    /// Filtered pages already walked, per tip and path. Immutable keys, so never stale.
    private struct PathHistoryKey: Hashable {
        let tip: Oid
        let path: String
    }
    private var pathHistoryCache:
        [PathHistoryKey: (commits: [CommitSummary], scanned: Int, complete: Bool)] = [:]
    /// Flattened file lists, per root tree. Content-addressed like every other cache.
    private var pathIndices: [Oid: PathIndex] = [:]
    private var indexBuild: (root: Oid, task: Task<PathIndex, any Error>)?
    /// Files on disk for path search in the working tree. Disk is not content-addressed, so
    /// this is dropped every time the search opens rather than cached by id.
    private var worktreeIndex: PathIndex?
    private var worktreeIndexBuild: (generation: Int, task: Task<PathIndex, any Error>)?
    private var worktreeIndexGeneration = 0
    /// Syntax spans, per blob. Content-addressed like every other cache, but bounded: span
    /// arrays for large files run to megabytes, so the least recently used entries are
    /// evicted past `Self.highlightCacheLimit` files or `Self.highlightCacheSpanLimit` spans.
    private var highlightCache: [Oid: [HighlightSpan]] = [:]
    /// Least recently used first.
    private var highlightCacheOrder: [Oid] = []
    private var highlightCacheSpanCount = 0
    /// Import links, per blob. They live and die with the blob's highlight spans, so the
    /// bounds above cover them too and they need no bookkeeping of their own.
    private var importLinkCache: [Oid: [ImportLink]] = [:]
    static let highlightCacheLimit = 16
    static let highlightCacheSpanLimit = 1_500_000

    public init() {}

    /// Attaches a repository and loads refs and HEAD, then selects the branch HEAD is on.
    public func attach(_ repository: any RepositoryReading) async {
        beginMove()
        defer { endMove() }
        self.repository = repository
        trees.removeAll()
        historyCache.removeAll()
        pathHistoryCache.removeAll()
        pathIndices.removeAll()
        highlightCache.removeAll()
        highlightCacheOrder.removeAll()
        highlightCacheSpanCount = 0
        importLinkCache.removeAll()
        indexBuild?.task.cancel()
        indexBuild = nil
        forgetWorktreePaths()
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
            if repository.workingDirectory != nil, head.target != nil {
                await selectWorkingTree()
            } else if let current = initialRef(listing: listing, head: head) {
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
        beginMove()
        defer { endMove() }
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
        beginMove()
        defer { endMove() }
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
        beginMove()
        defer { endMove() }
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
        beginMove()
        defer { endMove() }
        selectedRef = nil
        browsedCommit = nil
        browsingWorkingTree = true
        onRefsChanged?()
        resetHistory(anchor: target)
        await loadRoot(of: target)
    }

    /// Shows or hides the entries `HiddenFiles` names in the tree and path search. Nothing
    /// reloads: the tree keeps every row it read and asks `isVisible` again.
    public func setShowsHiddenFiles(_ shows: Bool) {
        showsHiddenFiles = shows
    }

    /// Whether a tree row is on screen under the current hidden-files setting. A row's name
    /// is enough: a hidden folder's children are never shown, because the folder is not.
    public func isVisible(_ row: BrowseRow) -> Bool {
        showsHiddenFiles || !HiddenFiles.isHidden(name: row.entry.name)
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
                        name: entry.name, oid: entry.oid ?? .unhashed, kind: entry.kind),
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
        let entry = TreeEntry(name: name, oid: .unhashed, kind: .blob)
        fileGeneration += 1
        let generation = fileGeneration
        openedFilePath = path
        fileState = .loading(entry)
        onFileChanged?()
        noteLocation()
        let result = await RepositoryError.run {
            let content = try repository.worktreeFile(path: path, limit: limit)
            return Self.state(for: entry, content: content)
        }
        guard generation == fileGeneration else { return }
        switch result {
        case let .success(state):
            fileState = state
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
        forgetWorktreePaths()
        let result = await RepositoryError.run {
            try repository.treeOf(target)
        }
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
            try store.entries(of: tree, read: repository.treeEntries)
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
        noteLocation()
        let result = await RepositoryError.run {
            let content = try repository.readBlob(entry.oid, limit: limit)
            return Self.state(for: entry, content: content)
        }
        guard generation == fileGeneration else { return }
        switch result {
        case let .success(state):
            fileState = state
        case let .failure(error):
            fileState = .failed(entry, error.message)
        }
        onFileChanged?()
    }

    /// Opens the file at a repository-relative path, the way a markdown link in the file pane
    /// asks for one. Returns `false` when the path names nothing openable, so the caller can
    /// say the link is dead instead of leaving the pane on the previous file.
    ///
    /// Resolution reads one tree object per path component, so a deep link costs its depth and
    /// not the size of the tree; every directory it reads lands in the shared cache.
    @discardableResult
    public func openPath(_ path: String) async -> Bool {
        guard repository != nil, !path.isEmpty else { return false }
        if browsingWorkingTree {
            let name = path.split(separator: "/").last.map(String.init) ?? path
            await openWorktreeFile(name: name, path: path)
            if case .failed = fileState { return false }
            return true
        }
        guard let root = rootTree, let entry = try? await blob(at: path, in: root) else {
            return false
        }
        await openFile(entry, path: path)
        return true
    }

    /// The blob at `path` below `tree`, walking one directory at a time. `nil` when a
    /// component is missing or the path names a directory.
    private func blob(at path: String, in tree: Oid) async throws -> TreeEntry? {
        let components = path.split(separator: "/").map(String.init)
        guard !components.isEmpty else { return nil }
        var current = tree
        for (offset, component) in components.enumerated() {
            let entries = try await children(of: current)
            guard let match = entries.first(where: { $0.name == component }) else { return nil }
            if offset == components.count - 1 {
                return match.kind.isDirectory ? nil : match
            }
            guard match.kind.isDirectory else { return nil }
            current = match.oid
        }
        return nil
    }

    // MARK: History

    /// Fetches the next page of commits for the history pane. Does nothing while a page is
    /// loading, after the last page, or when nothing is selected. Pages accumulate in a
    /// cache keyed by the tip — and the path when scoped — so switching away and back does
    /// not re-walk.
    public func loadMoreHistory() async {
        switch historyScope {
        case .all:
            await loadMoreLog()
        case let .path(path):
            await loadMorePathHistory(path)
        }
    }

    private func loadMoreLog() async {
        guard let repository, let tip = historyAnchor, !isLoadingHistory, !historyComplete
        else { return }
        isLoadingHistory = true
        let generation = historyGeneration
        let skip = history.count
        let pageSize = Self.historyPageSize
        let result = await RepositoryError.run {
            try repository.log(tip: tip, skip: skip, limit: pageSize)
        }
        guard generation == historyGeneration else { return }
        isLoadingHistory = false
        switch result {
        case let .success(page):
            history += page
            historyComplete = page.count < pageSize
            graph.append(page[...])
            historyCache[tip] = HistoryPages(
                commits: history, complete: historyComplete, graph: graph)
            onHistoryChanged?()
        case let .failure(error):
            fail(error.message)
        }
    }

    /// The lane graph rows aligned with `history`. Empty while a path scope or search shows
    /// a filtered list, whose rows have no meaningful topology.
    public var graphRows: [GraphRow] {
        guard historyScope == .all, searchFilter == nil else { return [] }
        return graph.rows
    }

    /// The widest the current graph has been, for sizing the graph column.
    public var graphMaxLaneCount: Int { graph.maxLaneCount }

    /// One page of path-scoped history, walked in scan chunks so a long stretch without a
    /// match never blocks a repository handle and a scope change between chunks drops the
    /// rest.
    private func loadMorePathHistory(_ path: String) async {
        guard let repository, let tip = historyAnchor, !isLoadingHistory, !historyComplete
        else { return }
        isLoadingHistory = true
        let generation = historyGeneration
        let key = PathHistoryKey(tip: tip, path: path)
        let target = history.count + Self.historyPageSize
        let chunk = Self.pathHistoryScanChunk
        while history.count < target, !historyComplete {
            let skip = pathHistoryCache[key]?.scanned ?? 0
            let remaining = target - history.count
            let result = await RepositoryError.run {
                try repository.pathHistory(
                    tip: tip, path: path, scanSkip: skip, limit: remaining,
                    maxScan: chunk)
            }
            guard generation == historyGeneration else { return }
            switch result {
            case let .success(page):
                history += page.commits
                historyComplete = page.complete
                pathHistoryCache[key] = (history, skip + page.scanned, page.complete)
                onHistoryChanged?()
            case let .failure(error):
                // Keep the commits already shown and stop paging, so a scroll or resize does
                // not re-walk into the same failure. Not cached: scoping again retries.
                isLoadingHistory = false
                historyComplete = true
                onHistoryChanged?()
                fail(error.message)
                return
            }
        }
        isLoadingHistory = false
    }

    /// Scopes the history pane to the commits that changed `path`, keeping the current
    /// anchor. Cached pages for the same tip and path come back without a walk.
    public func showPathHistory(_ path: String) async {
        guard scopeHistory(to: path), !historyComplete else { return }
        await loadMoreHistory()
    }

    /// The half of `showPathHistory` that reads nothing: the scope changes and the cached
    /// pages show. Answers `false` when there is no history to scope.
    private func scopeHistory(to path: String) -> Bool {
        guard repository != nil, let tip = historyAnchor else { return false }
        historyGeneration += 1
        isLoadingHistory = false
        historyScope = .path(path)
        clearSearch()
        clearCommitDetail()
        if let cached = pathHistoryCache[PathHistoryKey(tip: tip, path: path)] {
            history = cached.commits
            historyComplete = cached.complete
        } else {
            history = []
            historyComplete = false
        }
        onHistoryChanged?()
        noteLocation()
        return true
    }

    /// Returns the history pane to the unfiltered list of the current anchor.
    public func clearHistoryScope() {
        guard historyScope != .all else { return }
        resetHistory(anchor: historyAnchor)
        noteLocation()
    }

    // MARK: Search

    /// Starts a search over the current anchor's history, streaming matches in scan chunks.
    /// An empty filter clears instead. A newer search, scope change, or ref switch between
    /// chunks drops the rest of the walk.
    public func search(_ filter: CommitFilter) async {
        guard repository != nil, historyAnchor != nil else { return }
        if filter.isEmpty {
            clearSearch()
            return
        }
        searchGeneration += 1
        searchFilter = filter
        searchResults = []
        searchScanned = 0
        searchComplete = false
        isSearching = true
        onSearchChanged?()
        await continueSearch(target: Self.historyPageSize)
    }

    /// Fetches the next page of matches for the active search.
    public func loadMoreSearchResults() async {
        guard searchFilter != nil, !isSearching, !searchComplete else { return }
        isSearching = true
        onSearchChanged?()
        await continueSearch(target: searchResults.count + Self.historyPageSize)
    }

    /// Drops the active search; the plain history is still loaded and shows again at once.
    public func clearSearch() {
        searchGeneration += 1
        let hadSearch = searchFilter != nil
        searchFilter = nil
        searchResults = []
        searchScanned = 0
        searchComplete = false
        isSearching = false
        if hadSearch {
            onSearchChanged?()
        }
    }

    /// Resolves a revision — an abbreviated hash, a ref name, anything `rev-parse` accepts —
    /// and shows that commit's detail.
    public func jumpToRevision(_ spec: String) async {
        guard let repository else { return }
        let result = await RepositoryError.run {
            try repository.resolve(spec)
        }
        switch result {
        case let .success(oid):
            await jumpToCommit(oid)
        case let .failure(error):
            onInputError?(Self.revisionNotFoundTitle, error.message)
        }
    }

    /// Title for a revision the jump field could not resolve.
    public static let revisionNotFoundTitle = "No such revision"

    private func continueSearch(target: Int) async {
        guard let repository, let tip = historyAnchor, let filter = searchFilter else { return }
        let generation = searchGeneration
        let chunk = Self.searchScanChunk
        while searchResults.count < target, !searchComplete {
            let skip = searchScanned
            let remaining = target - searchResults.count
            let result = await RepositoryError.run {
                try repository.searchCommits(
                    tip: tip, filter: filter, scanSkip: skip, limit: remaining,
                    maxScan: chunk)
            }
            guard generation == searchGeneration else { return }
            switch result {
            case let .success(page):
                searchResults += page.commits
                searchScanned += page.scanned
                searchComplete = page.complete
                onSearchChanged?()
            case let .failure(error):
                isSearching = false
                onSearchChanged?()
                fail(error.message)
                return
            }
        }
        guard generation == searchGeneration else { return }
        isSearching = false
        onSearchChanged?()
    }

    private func resetHistory(anchor: Oid?) {
        historyGeneration += 1
        isLoadingHistory = false
        historyAnchor = anchor
        historyScope = .all
        clearSearch()
        clearCommitDetail()
        if let anchor, let cached = historyCache[anchor] {
            history = cached.commits
            historyComplete = cached.complete
            graph = cached.graph
        } else {
            history = []
            historyComplete = false
            graph = GraphBuilder()
        }
        onHistoryChanged?()
    }

    /// Closes the commit detail: returns the detail state to `.empty`, so the view can
    /// dismiss its drawer.
    public func closeCommitDetail() {
        clearCommitDetail()
        noteLocation()
    }

    private func clearCommitDetail() {
        diffGeneration += 1
        if diffCommit != nil {
            diffCommit = nil
            commitChanges = []
            fileDiff = .empty
            commitDetail = .empty
            onDiffChanged?()
        }
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
        noteLocation()
        let parent = commit.parents.first
        let target = commit.oid
        let result = await RepositoryError.run {
            let changes = try repository.diffTrees(from: parent, to: target)
            let detail = try repository.commitDetails(target)
            return (changes, detail)
        }
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

    /// Shows the detail of a commit reached by id — a parent link, a revision jump, or a
    /// stop of the back/forward list.
    public func jumpToCommit(_ oid: Oid) async {
        guard let summary = await readCommit(oid) else { return }
        await loadCommitChanges(summary)
    }

    /// One commit's summary, read off the main actor. A commit that cannot be read is
    /// reported and answers `nil`.
    private func readCommit(_ oid: Oid) async -> CommitSummary? {
        guard let repository else { return nil }
        let result = await RepositoryError.run {
            try repository.commit(oid)
        }
        switch result {
        case let .success(summary):
            return summary
        case let .failure(error):
            fail(error.message)
            return nil
        }
    }

    /// Loads the line diff of one changed file from `commitChanges`.
    public func loadFileDiff(_ change: TreeChange) async {
        guard let repository else { return }
        diffGeneration += 1
        let generation = diffGeneration
        fileDiff = .loading(change)
        onDiffChanged?()
        let result = await RepositoryError.run {
            try repository.diffBlobs(
                old: change.old, new: change.new,
                limit: Repository.defaultBlobLimit, context: 3)
        }
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

    // MARK: Location

    /// Where the browser is, as a value the back/forward list can return to.
    public var location: BrowserLocation {
        let tree: BrowsedTree
        if browsingWorkingTree {
            tree = .workingTree
        } else if let selectedRef {
            tree = .ref(selectedRef.fullName)
        } else if let browsedCommit {
            tree = .commit(browsedCommit.oid, historyTip: historyAnchor)
        } else {
            tree = .nothing
        }
        return BrowserLocation(
            tree: tree, filePath: openedFilePath, historyScope: historyScope,
            detailCommit: diffCommit?.oid)
    }

    /// Returns the browser to a location it was at, changing only what differs: a step
    /// between two files of one tree reads one blob and nothing else. The history pane's page
    /// is left to the pane, which asks for one when it is on screen.
    ///
    /// What the location names is read as it is now, so a ref that moved shows its new tip and
    /// a file that is gone leaves the pane empty. A tree that cannot be shown any more — a
    /// deleted ref, a pruned commit — is reported and ends the restore with nothing else
    /// changed, and so does a ref picked on the way.
    ///
    /// Restores never overlap. One asked for while another runs returns at once; the running
    /// one stops at its next step and goes on to the newest location asked for, so two quick
    /// steps back end on the second whichever load finishes first.
    public func restore(_ target: BrowserLocation) async {
        guard repository != nil else { return }
        pendingRestore = target
        guard !isRestoring else { return }
        isRestoring = true
        beginMove()
        defer {
            isRestoring = false
            endMove()
        }
        while let next = pendingRestore {
            pendingRestore = nil
            await move(to: next)
        }
    }

    private func move(to target: BrowserLocation) async {
        if location.tree != target.tree {
            await restoreTree(target.tree)
            guard pendingRestore == nil, location.tree == target.tree else { return }
        }
        if openedFilePath != target.filePath {
            var opened = false
            if let path = target.filePath {
                opened = await openPath(path)
            }
            // A file that is gone empties the pane: the file of the stop left behind, still
            // open, would read as the one this stop named.
            if !opened {
                closeFile()
            }
            guard pendingRestore == nil else { return }
        }
        if historyScope != target.historyScope {
            switch target.historyScope {
            case .all:
                clearHistoryScope()
            case let .path(path):
                _ = scopeHistory(to: path)
            }
        }
        if diffCommit?.oid != target.detailCommit {
            if let commit = target.detailCommit {
                await jumpToCommit(commit)
            } else {
                closeCommitDetail()
            }
        }
    }

    private func restoreTree(_ tree: BrowsedTree) async {
        switch tree {
        case .nothing:
            break
        case .workingTree:
            await selectWorkingTree()
        case let .ref(name):
            guard let ref = refs.refs.first(where: { $0.fullName == name }) else {
                fail("cannot return to \(name): the ref no longer exists")
                return
            }
            await select(ref)
        case let .commit(oid, historyTip):
            let rootToken = rootGeneration
            guard let summary = await readCommit(oid),
                // Another tree was picked during the read.
                rootToken == rootGeneration
            else { return }
            await browseCommit(summary)
            if historyAnchor != historyTip {
                resetHistory(anchor: historyTip)
            }
        }
    }

    /// Empties the file pane, for a location that had no file open.
    private func closeFile() {
        fileGeneration += 1
        fileState = .empty
        openedFilePath = nil
        onFileChanged?()
        noteLocation()
    }

    private func beginMove() {
        moveDepth += 1
    }

    private func endMove() {
        moveDepth -= 1
        noteLocation()
    }

    /// Fires `onLocationChanged` when the location differs from the last one reported and
    /// no move is under way.
    private func noteLocation() {
        guard moveDepth == 0 else { return }
        let current = location
        guard current != reportedLocation else { return }
        reportedLocation = current
        onLocationChanged?()
    }

    // MARK: Highlighting

    /// Syntax spans for a text file, computed in the background and cached by blob id, so a
    /// file parses once however often it is reopened. `nil` for unknown languages, files
    /// whose bytes did not decode cleanly (offsets would misalign), or oversized files.
    public func highlights(for entry: TreeEntry, document: TextDocument) async
        -> [HighlightSpan]?
    {
        guard !document.isLossy, document.byteCount <= Highlighter.byteLimit else { return nil }
        let cacheable = !entry.oid.isUnhashed
        if cacheable, let cached = highlightCache[entry.oid] {
            if let index = highlightCacheOrder.firstIndex(of: entry.oid) {
                highlightCacheOrder.remove(at: index)
                highlightCacheOrder.append(entry.oid)
            }
            return cached
        }
        let name = entry.name
        let text = document.text
        let spans = await Task.detached(priority: .utility) {
            Highlighter.highlight(name: name, data: Array(text.utf8))
        }.value
        if cacheable, let spans {
            storeHighlights(spans, for: entry.oid)
        }
        return spans
    }

    /// Inserts one entry and evicts from the least recently used end until both caps hold.
    private func storeHighlights(_ spans: [HighlightSpan], for oid: Oid) {
        if highlightCache[oid] == nil {
            highlightCacheOrder.append(oid)
            highlightCacheSpanCount += spans.count
            highlightCache[oid] = spans
        }
        while highlightCacheOrder.count > Self.highlightCacheLimit
            || highlightCacheSpanCount > Self.highlightCacheSpanLimit
        {
            guard highlightCacheOrder.count > 1 else { break }
            let evicted = highlightCacheOrder.removeFirst()
            highlightCacheSpanCount -= highlightCache.removeValue(forKey: evicted)?.count ?? 0
            importLinkCache.removeValue(forKey: evicted)
        }
    }

    // MARK: Import links

    /// The import links of a text file, found in the background. Cached by blob id for as
    /// long as the blob's highlight spans are, so going back and forth between two files
    /// parses neither again. `nil` under the same conditions as `highlights`.
    public func importLinks(for entry: TreeEntry, document: TextDocument) async -> [ImportLink]? {
        guard !document.isLossy, document.byteCount <= Highlighter.byteLimit else { return nil }
        let cacheable = !entry.oid.isUnhashed
        if cacheable, let cached = importLinkCache[entry.oid] { return cached }
        let name = entry.name
        let text = document.text
        let links = await Task.detached(priority: .utility) {
            ImportLinks.find(name: name, text: text)
        }.value
        if cacheable, let links, highlightCache[entry.oid] != nil {
            importLinkCache[entry.oid] = links
        }
        return links
    }

    /// Opens the file a module specifier names, the way an import link in the file pane asks
    /// for one. `sourcePath` is the repository-relative path of the file the specifier is
    /// written in. Returns `false` when the specifier names nothing in this tree, which is
    /// what a package from a registry does.
    ///
    /// Resolution reads the directories on the way to each candidate, through the shared tree
    /// cache, and the config files above `sourcePath` when the specifier is not relative. It
    /// keeps nothing: a click is rare, and a config differs from one tree to the next.
    @discardableResult
    public func openImport(_ specifier: String, from sourcePath: String) async -> Bool {
        guard let repository, let root = rootTree else { return false }
        let fileToken = fileGeneration
        let rootToken = rootGeneration
        let onDisk = browsingWorkingTree
        let store = trees
        let path = await Task.detached(priority: .userInitiated) { () -> String? in
            let resolver =
                onDisk
                ? Self.worktreeResolver(repository: repository, root: root)
                : Self.treeResolver(repository: repository, root: root, store: store)
            return resolver.resolve(specifier, from: sourcePath)
        }.value
        // The user opened something else, or switched ref, while this resolved.
        guard fileToken == fileGeneration, rootToken == rootGeneration else { return true }
        guard let path else { return false }
        return await openPath(path)
    }

    /// A resolver over a committed tree: one tree object per directory on the way to a path,
    /// each landing in the shared cache.
    nonisolated private static func treeResolver(
        repository: any RepositoryReading, root: Oid, store: TreeStore
    ) -> ImportResolver {
        let list: (String) -> [TreeEntry]? = { path in
            var current = root
            for component in path.split(separator: "/") {
                let entries = try? store.entries(of: current, read: repository.treeEntries)
                guard let match = entries?.first(where: { $0.name == component }),
                    match.kind.isDirectory
                else { return nil }
                current = match.oid
            }
            return try? store.entries(of: current, read: repository.treeEntries)
        }
        return ImportResolver(list: list) { path in
            let parent = path.split(separator: "/").dropLast().joined(separator: "/")
            let name = path.split(separator: "/").last.map(String.init)
            guard let entry = list(parent)?.first(where: { $0.name == name }),
                let content = try? repository.readBlob(entry.oid, limit: importConfigLimit),
                case let .text(data, _, _) = content
            else { return nil }
            return data
        }
    }

    /// A resolver over the files on disk, for the working tree.
    nonisolated private static func worktreeResolver(
        repository: any RepositoryReading, root: Oid
    ) -> ImportResolver {
        ImportResolver(
            list: { path in
                guard let listing = try? repository.worktreeEntries(tree: root, dir: path) else {
                    return nil
                }
                return listing.entries.map {
                    TreeEntry(name: $0.name, oid: $0.oid ?? .unhashed, kind: $0.kind)
                }
            },
            read: { path in
                guard
                    let content = try? repository.worktreeFile(
                        path: path, limit: importConfigLimit),
                    case let .text(data, _, _) = content
                else { return nil }
                return data
            })
    }

    /// The most a `tsconfig.json` may weigh and still be read for its module paths.
    nonisolated static let importConfigLimit: UInt64 = 256 * 1024

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
                try store.entries(of: tree, read: repository.treeEntries)
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

    /// The best matches for `query` among every file below the current root — on disk while
    /// browsing the working tree, so gitignored files such as `.env` are found too. Builds the
    /// index on first use; a query while a build for another root is running cancels it.
    /// Hidden files match only while `showsHiddenFiles` is on.
    public func searchPaths(_ query: String, limit: Int = 100) async throws -> [PathIndexEntry] {
        guard let root = rootTree else { return [] }
        let index =
            browsingWorkingTree
            ? try await worktreePathIndex(comparedTo: root) : try await pathIndex(for: root)
        let includeHidden = showsHiddenFiles
        return await Task.detached(priority: .userInitiated) {
            index.search(query, limit: limit, includeHidden: includeHidden)
        }.value
    }

    /// Drops the working tree's path index so the next search walks the disk again. Called
    /// each time path search opens: files appear and vanish without any ref moving.
    public func forgetWorktreePaths() {
        worktreeIndexGeneration += 1
        worktreeIndex = nil
        worktreeIndexBuild = nil
    }

    /// Every file on disk, walked once per path-search session. Queries during a walk share
    /// it; a walk that `forgetWorktreePaths` outdated still answers the query that started it
    /// but is not kept, and a failed walk is not kept either, so the next query retries.
    private func worktreePathIndex(comparedTo root: Oid) async throws -> PathIndex {
        if let worktreeIndex { return worktreeIndex }
        let generation = worktreeIndexGeneration
        if let build = worktreeIndexBuild, build.generation == generation {
            return try await build.task.value
        }
        guard let repository else { throw RepositoryError("no repository is open") }
        let task = Task.detached(priority: .userInitiated) {
            let list = try repository.worktreeFiles(tree: root, maxScan: PathIndex.maxEntries)
            return PathIndex(root: root, worktreeFiles: list)
        }
        worktreeIndexBuild = (generation, task)
        defer {
            if worktreeIndexBuild?.generation == generation { worktreeIndexBuild = nil }
        }
        let index = try await task.value
        if generation == worktreeIndexGeneration { worktreeIndex = index }
        return index
    }

    /// Opens a path-search pick: from disk while browsing the working tree, else the blob.
    public func openSearchResult(_ entry: PathIndexEntry) async {
        if browsingWorkingTree {
            await openWorktreeFile(name: entry.name, path: entry.path)
        } else {
            let treeEntry = TreeEntry(name: entry.name, oid: entry.oid, kind: entry.kind)
            await openFile(treeEntry, path: entry.path)
        }
    }

    // MARK: Helpers

    /// Maps blob content to a pane state. Pure and nonisolated: building a `TextDocument`
    /// walks the whole content, so the file-open paths call this off the main actor.
    nonisolated public static func state(for entry: TreeEntry, content: BlobContent) -> FileState {
        switch content {
        case let .text(data, lineStarts, validUTF8):
            return .text(
                entry, TextDocument(bytes: data, lineStarts: lineStarts, validUTF8: validUTF8))
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
            return .failure(RepositoryError(wrapping: error))
        }
    }

    private func fail(_ message: String) {
        lastError = message
        onError?(message)
    }
}
