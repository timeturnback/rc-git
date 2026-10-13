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

/// Where the open file's bytes came from: a working-tree file read by path, or a blob.
private enum OpenedFileSource {
    /// Read from disk; `status` is how the file relates to HEAD, `nil` when the opener did
    /// not know.
    case disk(name: String, status: WorktreeStatus?)
    case blob(TreeEntry)
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

    /// The diff pane's line while no file diff is shown. Only a loaded commit with no
    /// changed files says "No files changed"; a load in flight or a failed one never does.
    public func emptyDiffPlaceholder(changeCount: Int) -> String {
        switch self {
        case .empty, .failed: ""
        case .loading: "Loading…"
        case .loaded: changeCount == 0 ? "No files changed" : "Select a file"
        }
    }
}

/// How `openPath` ended: the file is open, nothing openable lives at the path, or reading
/// the trees or directories on the way failed with the core's message.
public enum OpenPathOutcome: Equatable, Sendable {
    case opened
    case missing
    case failed(String)
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
    /// What the tree says while `rootTree` is `nil`.
    public var treePlaceholder: String {
        TreePlaceholder.text(attached: repository != nil, head: head)
    }
    public private(set) var fileState: FileState = .empty
    /// Repository-relative path of the file in the file pane, when the opener knew it.
    public private(set) var openedFilePath: String?
    /// Whether every component of `openedFilePath` is the stored name byte for byte. When it
    /// is not, the path carries U+FFFD replacements and the core would resolve it to whatever
    /// has that spelling, so blame and file history refuse it.
    public private(set) var openedPathIsExact = true
    /// The `#fragment` of the repository link that opened the file loading or on screen, for
    /// the pane to scroll to once it paints. Every other open, a dead link included, clears it,
    /// so a fragment never lands on a file it was not asked for.
    public private(set) var pendingFragment: String?
    /// The fragment `openPath` hands to the load it is about to start; `loadFile` moves it to
    /// `pendingFragment`, so a load started any other way clears that instead.
    private var fragmentForNextLoad: String?
    /// The size limit the file pane last read with, which blame honors too.
    private var openedFileLimit = Repository.defaultBlobLimit
    /// Where the file in the file pane was read from, so `loadAnyway` re-reads the same place.
    private var openedFileSource: OpenedFileSource?
    /// True after a ref refresh failed: `refs`, `head` and the selection are the last listing
    /// that read, kept but no longer known to be current. The next successful read clears it.
    public private(set) var refsStale = false
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
    /// The blame `requestBlame` last started or finished for the open file.
    public private(set) var blame: BlameState = .empty

    /// Commits of the selected ref loaded so far, newest first.
    public private(set) var history: [CommitSummary] = []
    /// True once a page came back shorter than `historyPageSize`, or a filtered walk ended.
    public private(set) var historyComplete = false
    public private(set) var isLoadingHistory = false
    /// Why the last history page failed, or `nil`. Paging refuses while it is set, so a
    /// scroll or resize does not re-walk into the same failure; `retryHistory` clears it.
    public private(set) var historyError: String?
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
    /// Why the active search stopped, or `nil`. Paging refuses while it is set; a new
    /// filter or `clearSearch` clears it.
    public private(set) var searchError: String?

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
    /// Fired after `blame` changed.
    public var onBlameChanged: (() -> Void)?
    /// Fired when a background operation failed with a message worth showing.
    public var onError: ((String) -> Void)?
    /// Fired when something the user typed named nothing in the repository: a title for what
    /// was wrong, then the detail. Not a read failure, so it never fires `onError`.
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
    private var blameGeneration = 0
    /// The file and revision the running blame walk is for; a request for the same joins it.
    private var blameInFlight: BlameTarget?
    /// A restore is running, and the location the next one goes to. A restore asked for
    /// meanwhile replaces `pendingRestore` and returns; the running one picks it up.
    private var isRestoring = false
    private var pendingRestore: BrowserLocation?
    /// The ref refresh running, and whether another is wanted. Refreshes and restores share
    /// one queue: a restore waits for `refreshRun`, and a refresh asked for during a restore
    /// sets `refreshPending` for the restore to start once it ends.
    private var refreshRun: Task<Void, Never>?
    private var refreshPending = false
    /// How many moves are under way. `location` is half changed while one is.
    private var moveDepth = 0
    /// The location `onLocationChanged` last fired for.
    private var reportedLocation = BrowserLocation()
    /// The tip whose history the history pane shows. Kept while browsing one of its commits.
    private var historyAnchor: Oid?
    /// One blame walk's inputs, plus the file generation it was asked under.
    private struct BlameTarget: Equatable, Sendable {
        let path: String
        let tip: Oid
        let limit: UInt64
        let file: Int
        /// True when the tip is HEAD standing in for the working tree, not a browsed revision.
        let atHead: Bool
    }
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
    /// Syntax spans, per blob and file extension. Content-addressed like every other cache,
    /// but bounded: span arrays for large files run to megabytes, so the least recently used
    /// entries are evicted past `highlightCacheLimit` files or `highlightCacheSpanLimit` spans.
    private var highlightCache: [HighlightKey: [HighlightSpan]] = [:]
    /// Least recently used first.
    private var highlightCacheOrder: [HighlightKey] = []
    private var highlightCacheSpanCount = 0
    /// Import links, per blob and file extension. They live and die with the entry's highlight
    /// spans, so the bounds above cover them too and they need no bookkeeping of their own.
    private var importLinkCache: [HighlightKey: [ImportLink]] = [:]
    let highlightCacheLimit: Int
    let highlightCacheSpanLimit: Int

    public convenience init() {
        self.init(highlightCacheLimit: 16, highlightCacheSpanLimit: 1_500_000)
    }

    /// A model with its highlight cache bounded at `highlightCacheLimit` files and
    /// `highlightCacheSpanLimit` spans, so tests can reach both caps with small inputs.
    init(highlightCacheLimit: Int, highlightCacheSpanLimit: Int) {
        self.highlightCacheLimit = highlightCacheLimit
        self.highlightCacheSpanLimit = highlightCacheSpanLimit
    }

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
        pendingFragment = nil
        openedFilePath = nil
        openedFileSource = nil
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
            refsStale = false
            onRefsChanged?()
            if repository.workingDirectory != nil, head.target != nil {
                await selectWorkingTree()
            } else if let current = initialRef(listing: listing, head: head) {
                await select(current)
            } else {
                rootTree = nil
                onRootChanged?()
            }
        case let .failure(error):
            rootTree = nil
            onRootChanged?()
            fail(error.message)
        }
    }

    /// Reloads refs and HEAD after the repository changed on disk, keeping every object
    /// cache: only refs can move. The selection survives by name; when its target moved the
    /// root tree and history reload, and when it disappeared the selection falls back the
    /// same way `attach` chooses one. A failed refresh keeps the last good listing, marks it
    /// stale through `refsStale` and `onRefsChanged`, and does not alert: the watcher fires
    /// during ref updates, when transient lock errors are normal.
    ///
    /// Refreshes never overlap each other or a restore. One asked for while another runs
    /// joins it, and the running one reads the refs once more when it lands. One asked for
    /// during a restore returns at once and runs after the restore, so its root load never
    /// replaces the root the restore is opening a file against.
    public func refreshRefs() async {
        guard repository != nil else { return }
        refreshPending = true
        guard !isRestoring else { return }
        if let refreshRun {
            await refreshRun.value
            return
        }
        let run = Task { await self.runRefreshes() }
        refreshRun = run
        await run.value
    }

    /// Reads the refs until no refresh is pending, stopping early for a restore that waits.
    private func runRefreshes() async {
        while refreshPending, !isRestoring {
            refreshPending = false
            await reloadRefs()
        }
        refreshRun = nil
    }

    private func reloadRefs() async {
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
            refsStale = false
            if browsingWorkingTree {
                await refreshWorkingTree(listing: listing, head: head, previousHead: previousHead)
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
        case .failure:
            // The message is dropped on purpose: a lock during a ref update is the usual cause
            // and heals on the next event, so the views show the quiet stale state instead.
            refsStale = true
            onRefsChanged?()
        }
    }

    /// Follows a moved HEAD while the working tree is browsed. An unborn HEAD has no tree to
    /// compare against, so the model leaves the working tree for the ref `attach` would pick.
    private func refreshWorkingTree(listing: RefListing, head: Head, previousHead: Oid?) async {
        guard head.target != nil else {
            browsingWorkingTree = false
            if let fallback = initialRef(listing: listing, head: head) {
                await select(fallback)
            } else {
                rootTree = nil
                resetHistory(anchor: nil)
                onRefsChanged?()
                onRootChanged?()
            }
            return
        }
        if head.target != previousHead {
            await selectWorkingTree()
        } else {
            onRefsChanged?()
        }
    }

    /// The branch `HEAD` is on, else the first ref pointing at `HEAD`'s commit, else the first
    /// local branch, else the first ref of any kind.
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
        let previous = currentSelection
        selectedRef = ref
        browsedCommit = nil
        browsingWorkingTree = false
        onRefsChanged?()
        resetHistory(anchor: ref.target)
        await loadRoot(of: ref.target, rollingBackTo: previous)
    }

    /// Shows the tree of one commit, picked from the history pane. The sidebar deselects and
    /// the history keeps showing the ref the commit came from, so hopping between commits
    /// stays one click.
    public func browseCommit(_ commit: CommitSummary) async {
        guard repository != nil else { return }
        beginMove()
        defer { endMove() }
        let previous = currentSelection
        selectedRef = nil
        browsedCommit = commit
        browsingWorkingTree = false
        onRefsChanged?()
        await loadRoot(of: commit.oid, rollingBackTo: previous)
    }

    /// Shows the files on disk with a status overlay against `HEAD`. Reads only; nothing in
    /// rc-git ever writes to the working tree.
    public func selectWorkingTree() async {
        guard repository != nil, let target = head?.target else { return }
        beginMove()
        defer { endMove() }
        let previous = currentSelection
        selectedRef = nil
        browsedCommit = nil
        browsingWorkingTree = true
        onRefsChanged?()
        resetHistory(anchor: target)
        await loadRoot(of: target, rollingBackTo: previous)
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
                        name: entry.name, oid: entry.oid ?? .unhashed, kind: entry.kind,
                        hasExactName: entry.hasExactName),
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

    /// Opens a file straight from the working directory into the file pane. `pathIsExact` is
    /// false when a component of `path` arrived with U+FFFD replacements; such a file is not
    /// read, since the read goes by path and could land on another file. `status` is the
    /// file's status against HEAD when the opener knows it; an untracked file cannot be blamed.
    public func openWorktreeFile(
        name: String, path: String, pathIsExact: Bool = true, status: WorktreeStatus? = nil,
        limit: UInt64 = Repository.defaultBlobLimit
    ) async {
        guard repository != nil else { return }
        let entry = TreeEntry(name: name, oid: .unhashed, kind: .blob, hasExactName: pathIsExact)
        openedFileSource = .disk(name: name, status: status)
        openedFilePath = path
        openedPathIsExact = pathIsExact
        openedFileLimit = limit
        await loadFile(entry) { repository in
            guard pathIsExact else {
                throw RepositoryError(InexactName.pathRefusal("read", path))
            }
            return try repository.worktreeFile(path: path, limit: limit)
        }
    }

    /// Opens a row of the file tree. A working-tree row that is still on disk reads from
    /// disk; every other row, a file deleted from disk included, reads its blob.
    public func openTreeRow(_ row: BrowseRow, path: String, pathIsExact: Bool = true) async {
        if browsingWorkingTree, !row.missing {
            await openWorktreeFile(
                name: row.entry.name, path: path, pathIsExact: pathIsExact, status: row.status)
        } else {
            await openFile(row.entry, path: path, pathIsExact: pathIsExact)
        }
    }

    /// The load steps every file open shares: a new generation, the loading state, a history
    /// stop, then `read` off the main actor, applied only while no newer open has started.
    /// The caller records the source, path and limit first.
    private func loadFile(
        _ entry: TreeEntry,
        read: @escaping @Sendable (any RepositoryReading) throws -> BlobContent
    ) async {
        guard let repository else { return }
        fileGeneration += 1
        let generation = fileGeneration
        pendingFragment = fragmentForNextLoad
        fragmentForNextLoad = nil
        fileState = .loading(entry)
        onFileChanged?()
        noteLocation()
        let result = await RepositoryError.run {
            let content = try read(repository)
            return Self.state(for: entry, content: content)
        }
        guard generation == fileGeneration else { return }
        switch result {
        case let .success(state):
            fileState = state
        case let .failure(error):
            pendingFragment = nil
            fileState = .failed(entry, error.message)
        }
        onFileChanged?()
    }

    /// What the sidebar and history show as browsed, kept so a root that cannot be read
    /// can hand the selection back to the tree still on screen.
    private struct Selection {
        let ref: RefEntry?
        let commit: CommitSummary?
        let workingTree: Bool
        let historyAnchor: Oid?
    }

    private var currentSelection: Selection {
        Selection(
            ref: selectedRef, commit: browsedCommit, workingTree: browsingWorkingTree,
            historyAnchor: historyAnchor)
    }

    /// Loads the root tree of a new selection. When the tree cannot be read (a ref to a blob,
    /// a tree missing from a partial clone) the selection rolls back to `previous`, so the
    /// sidebar, history and `location` keep naming the tree and file still on screen.
    private func loadRoot(of target: Oid, rollingBackTo previous: Selection) async {
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
            pendingFragment = nil
            openedFilePath = nil
            openedFileSource = nil
            onFileChanged?()
            onRootChanged?()
        case let .failure(error):
            rollBack(to: previous)
            fail(error.message)
        }
    }

    private func rollBack(to previous: Selection) {
        selectedRef = previous.ref
        browsedCommit = previous.commit
        browsingWorkingTree = previous.workingTree
        if historyAnchor != previous.historyAnchor {
            resetHistory(anchor: previous.historyAnchor)
        }
        onRefsChanged?()
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
    /// kept for blame; pass `nil` when it is unknown. `pathIsExact` is false when a folder on
    /// the way to the file arrived with U+FFFD replacements; the entry's own name counts too.
    public func openFile(
        _ entry: TreeEntry, path: String? = nil, pathIsExact: Bool = true,
        limit: UInt64 = Repository.defaultBlobLimit
    ) async {
        guard repository != nil, !entry.kind.isDirectory else { return }
        openedFileSource = .blob(entry)
        if let path {
            openedFilePath = path
            openedPathIsExact = pathIsExact && entry.hasExactName
        }
        openedFileLimit = limit
        await loadFile(entry) { repository in
            try repository.readBlob(entry.oid, limit: limit)
        }
    }

    /// Re-reads the file on screen with a larger `limit`, from where it was opened: the
    /// working directory for a file read from disk, its blob otherwise. A file deleted from
    /// disk opened through its HEAD blob, so `browsingWorkingTree` cannot decide this.
    public func loadAnyway(limit: UInt64) async {
        switch openedFileSource {
        case nil:
            return
        case let .disk(name, status):
            guard let path = openedFilePath else { return }
            await openWorktreeFile(
                name: name, path: path, pathIsExact: openedPathIsExact, status: status,
                limit: limit)
        case let .blob(entry):
            await openFile(
                entry, path: openedFilePath, pathIsExact: openedPathIsExact, limit: limit)
        }
    }

    /// Opens the file at a repository-relative path, the way a markdown link in the file pane
    /// asks for one. Answers `.missing` when the path names nothing openable, so the caller can
    /// say the link is dead, and `.failed` when a read on the way failed; either way the pane
    /// and `location` stay on the previous file. `fragment` becomes `pendingFragment` only once
    /// the file load starts; a dead or failed link leaves `pendingFragment` empty.
    ///
    /// Resolution reads one tree object, or one working-tree directory, per path component, so
    /// a deep link costs its depth and not the size of the tree; every tree object it reads
    /// lands in the shared cache.
    @discardableResult
    public func openPath(_ path: String, fragment: String? = nil) async -> OpenPathOutcome {
        pendingFragment = nil
        guard let repository, let root = rootTree, !path.isEmpty else { return .missing }
        if browsingWorkingTree {
            return await openWorktreePath(
                path, fragment: fragment, repository: repository, root: root)
        }
        let found: TreeEntry?
        do {
            found = try await blob(at: path, in: root)
        } catch {
            return .failed(RepositoryError(wrapping: error).message)
        }
        guard let entry = found else { return .missing }
        fragmentForNextLoad = fragment
        await openFile(entry, path: path)
        fragmentForNextLoad = nil
        return .opened
    }

    /// Hands the pane the fragment to scroll to once the file it was asked for has painted, and
    /// forgets it, so a repaint of the same file does not jump back.
    public func takePendingFragment() -> String? {
        defer { pendingFragment = nil }
        return pendingFragment
    }

    /// `openPath` over the working directory: confirms on disk that `path` names a file
    /// before the pane changes, so a dead link neither replaces the open file nor records a
    /// stop.
    private func openWorktreePath(
        _ path: String, fragment: String?, repository: any RepositoryReading, root: Oid
    ) async -> OpenPathOutcome {
        let lookup = await RepositoryError.run {
            try Self.worktreeFileStatus(at: path, repository: repository, root: root)
        }
        let status: WorktreeStatus
        switch lookup {
        case let .failure(error):
            return .failed(error.message)
        case .success(nil):
            return .missing
        case let .success(found?):
            status = found
        }
        let name = path.split(separator: "/").last.map(String.init) ?? path
        fragmentForNextLoad = fragment
        await openWorktreeFile(name: name, path: path, status: status)
        fragmentForNextLoad = nil
        if case let .failed(_, message) = fileState { return .failed(message) }
        return .opened
    }

    /// The status of the file `path` names in the working directory, listing one directory per
    /// component. `nil` when a component is missing or the path names a directory.
    nonisolated private static func worktreeFileStatus(
        at path: String, repository: any RepositoryReading, root: Oid
    ) throws -> WorktreeStatus? {
        let components = path.split(separator: "/").map(String.init)
        var dir = ""
        for (offset, component) in components.enumerated() {
            let listing = try repository.worktreeEntries(tree: root, dir: dir)
            // An inexact name only looks like `component`; the name on disk differs.
            guard
                let match = listing.entries.first(where: {
                    $0.hasExactName && $0.name == component
                })
            else { return nil }
            if offset == components.count - 1 {
                return match.kind.isDirectory ? nil : match.status
            }
            guard match.kind.isDirectory else { return nil }
            dir = dir.isEmpty ? component : dir + "/" + component
        }
        return nil
    }

    /// The blob at `path` below `tree`, walking one directory at a time. `nil` when a
    /// component is missing or the path names a directory.
    private func blob(at path: String, in tree: Oid) async throws -> TreeEntry? {
        let components = path.split(separator: "/").map(String.init)
        guard !components.isEmpty else { return nil }
        var current = tree
        for (offset, component) in components.enumerated() {
            let entries = try await children(of: current)
            // An inexact name only looks like `component`; the stored name differs.
            guard let match = entries.first(where: { $0.hasExactName && $0.name == component })
            else { return nil }
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
    /// not re-walk. Refuses after a failed page until `retryHistory`.
    public func loadMoreHistory() async {
        switch historyScope {
        case .all:
            await loadMoreLog()
        case let .path(path):
            await loadMorePathHistory(path)
        }
    }

    /// Clears a failed history page and asks for it again. Does nothing without a failure.
    public func retryHistory() async {
        guard historyError != nil else { return }
        historyError = nil
        onHistoryChanged?()
        await loadMoreHistory()
    }

    private func loadMoreLog() async {
        guard let repository, let tip = historyAnchor, !isLoadingHistory, !historyComplete,
            historyError == nil
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
            historyError = error.message
            onHistoryChanged?()
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
        guard let repository, let tip = historyAnchor, !isLoadingHistory, !historyComplete,
            historyError == nil
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
                // Keep the commits already shown; the error stops paging. Not cached:
                // scoping again or `retryHistory` retries.
                isLoadingHistory = false
                historyError = error.message
                onHistoryChanged?()
                fail(error.message)
                return
            }
        }
        isLoadingHistory = false
    }

    /// Scopes the history pane to the commits that changed `path`, keeping the current
    /// anchor. Cached pages for the same tip and path come back without a walk. A path whose
    /// `pathIsExact` is false is refused through `onError`: the walk matches by path, so it
    /// would show another file's history.
    public func showPathHistory(_ path: String, pathIsExact: Bool = true) async {
        guard pathIsExact else {
            onError?(InexactName.pathRefusal("show the history of", path))
            return
        }
        guard scopeHistory(to: path), !historyComplete else { return }
        await loadMoreHistory()
    }

    /// The half of `showPathHistory` that reads nothing: the scope changes and the cached
    /// pages show. Answers `false` when there is no history to scope.
    private func scopeHistory(to path: String) -> Bool {
        guard repository != nil, let tip = historyAnchor else { return false }
        historyGeneration += 1
        isLoadingHistory = false
        historyError = nil
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

    /// The text Copy Path puts on the pasteboard, or `nil` after refusing an inexact path
    /// through `onError`: its U+FFFD spelling names a different file, or none.
    public func copyablePath(_ path: String, pathIsExact: Bool) -> String? {
        guard pathIsExact else {
            onError?(InexactName.pathRefusal("copy the path of", path))
            return nil
        }
        return path
    }

    /// Returns the history pane to the unfiltered list of the current anchor.
    public func clearHistoryScope() {
        guard historyScope != .all else { return }
        resetHistory(anchor: historyAnchor)
        noteLocation()
    }

    // MARK: Search

    /// Starts a search over the current anchor's history, streaming matches in scan chunks.
    /// Under a path scope it searches only the commits that changed that path. An empty
    /// filter clears instead. A newer search, scope change, or ref switch between chunks
    /// drops the rest of the walk, and `onSearchChanged` reports the drop.
    public func search(_ filter: CommitFilter) async {
        guard repository != nil, historyAnchor != nil else { return }
        if filter.isEmpty {
            clearSearch()
            return
        }
        var filter = filter
        if case let .path(path) = historyScope {
            filter.path = path
        }
        searchGeneration += 1
        searchFilter = filter
        searchResults = []
        searchScanned = 0
        searchComplete = false
        searchError = nil
        isSearching = true
        onSearchChanged?()
        await continueSearch(target: Self.historyPageSize)
    }

    /// Fetches the next page of matches for the active search. Refuses after a failed chunk:
    /// only a new filter searches again.
    public func loadMoreSearchResults() async {
        guard searchFilter != nil, !isSearching, !searchComplete, searchError == nil else {
            return
        }
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
        searchError = nil
        isSearching = false
        if hadSearch {
            onSearchChanged?()
        }
    }

    /// Resolves a revision — an abbreviated hash, a ref name, anything `rev-parse` accepts —
    /// and shows that commit's detail. Returns false, reporting nothing, when the spec does not
    /// resolve or another commit was picked during the read.
    @discardableResult
    public func jumpToRevision(_ spec: String) async -> Bool {
        guard let repository else { return false }
        let generation = diffGeneration
        let result = await RepositoryError.run {
            try repository.resolve(spec)
        }
        // Another commit was picked during the read.
        guard generation == diffGeneration else { return false }
        switch result {
        case let .success(oid):
            await jumpToCommit(oid)
            return true
        case .failure:
            // Benign: the search box submits any hex-looking word ("2024", "added") as a
            // revision, and the message search for the same text is already running.
            return false
        }
    }

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
                searchError = error.message
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
        historyError = nil
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
            onDiffChanged?()
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
        let generation = diffGeneration
        guard let summary = await readCommit(oid),
            // Another commit was picked during the read.
            generation == diffGeneration
        else { return }
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

    /// Loads the line diff of one changed file from `commitChanges`. A submodule change has
    /// no blobs in this repository and settles without a read.
    public func loadFileDiff(_ change: TreeChange) async {
        guard let repository else { return }
        diffGeneration += 1
        let generation = diffGeneration
        if change.entry == .submodule {
            fileDiff = .submodule(change)
            onDiffChanged?()
            return
        }
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

    /// Blames the file in the file pane at the browsed revision and publishes the result as
    /// `blame`, firing `onBlameChanged`. Ranges are paired with their commit summaries,
    /// resolved through a cache so each commit is read once.
    ///
    /// The working tree has no revision of its own, so there the walk blames HEAD's committed
    /// version of the file, not the bytes on disk, and the panel title says so. An untracked
    /// file has no history and is refused.
    ///
    /// The walk stops at `blameFloor(tipTime:)` of the tip's committer time, so a line older
    /// than that comes back as a span with `olderThan` set instead of costing a walk of the
    /// whole ancestry; the walk itself cannot be cancelled.
    ///
    /// The file at the tip is sized against the limit the file pane read it with, so a file
    /// the pane declined as too large is declined here too instead of decoded whole. A request
    /// for the file and revision a walk is already running for joins that walk instead of
    /// starting another. `.loading` is published when a walk starts. A result is dropped when
    /// a newer request or `cancelBlame` superseded it; when the open file or revision changed
    /// while it ran, `.empty` is published instead. Failures publish `.failed`.
    public func requestBlame() async {
        guard let repository else { return publishBlame(.failed("no repository is open")) }
        guard let target = blameTarget() else {
            let message =
                openedFilePath == nil
                ? "no file with a known path is open" : "nothing is selected to blame at"
            return publishBlame(.failed(message))
        }
        guard !openedFileIsUntracked else {
            return publishBlame(
                .failed("\(target.path) is untracked; it has no history to blame"))
        }
        guard openedPathIsExact else {
            return publishBlame(.failed(InexactName.pathRefusal("blame", target.path)))
        }
        guard target != blameInFlight else { return }
        blameGeneration += 1
        let generation = blameGeneration
        blameInFlight = target
        publishBlame(.loading(path: target.path, title: Self.blamingTitle(path: target.path)))
        let result = await RepositoryError.run {
            try Self.blameSpans(repository, target: target)
        }
        guard generation == blameGeneration else { return }
        blameInFlight = nil
        guard target == blameTarget() else { return publishBlame(.empty) }
        switch result {
        case let .success(spans):
            let title = Self.blameTitle(path: target.path, atHead: target.atHead)
            publishBlame(.loaded(path: target.path, spans: spans, title: title))
        case let .failure(error):
            publishBlame(.failed(error.message))
        }
    }

    /// Drops the running blame walk's result, so it never reaches a closed window. A blame
    /// still loading becomes `.empty`; a finished one is left as it is.
    public func cancelBlame() {
        blameGeneration += 1
        blameInFlight = nil
        if case .loading = blame { publishBlame(.empty) }
    }

    /// Whether Show Blame can run on the file in the file pane: a file with a known path is
    /// open at a revision, which is HEAD in the working tree, and it is not untracked.
    public var canBlameOpenedFile: Bool {
        blameTarget() != nil && !openedFileIsUntracked
    }

    /// True when the open file was read from disk in the working tree and HEAD does not have it.
    private var openedFileIsUntracked: Bool {
        guard browsingWorkingTree, case .disk(_, .untracked) = openedFileSource else {
            return false
        }
        return true
    }

    /// What a blame asked for now would walk, or `nil` when no file with a known path is open
    /// at a committed revision. The file generation makes a reopen count as a different file.
    private func blameTarget() -> BlameTarget? {
        let tip = browsingWorkingTree ? head?.target : (selectedRef?.target ?? browsedCommit?.oid)
        guard let path = openedFilePath, let tip else { return nil }
        return BlameTarget(
            path: path, tip: tip, limit: openedFileLimit, file: fileGeneration,
            atHead: browsingWorkingTree)
    }

    /// The blame panel's title: the path, marked as HEAD's version when the working tree
    /// stood in for a revision.
    public nonisolated static func blameTitle(path: String, atHead: Bool) -> String {
        atHead ? "Blame — \(path) at HEAD" : "Blame — \(path)"
    }

    /// The blame panel's title while the walk for `path` runs.
    public nonisolated static func blamingTitle(path: String) -> String {
        "Blaming \(path)…"
    }

    /// How far back blame walks: two years (730 days), in seconds.
    public nonisolated static let blameWindow: Int64 = 730 * 86_400

    /// The `since` floor blame walks to from a tip committed at `tipTime` (seconds since the
    /// Unix epoch): `blameWindow` before it.
    public nonisolated static func blameFloor(tipTime: Int64) -> Int64 {
        tipTime - blameWindow
    }

    private func publishBlame(_ state: BlameState) {
        blame = state
        onBlameChanged?()
    }

    nonisolated private static func blameSpans(
        _ repository: any RepositoryReading, target: BlameTarget
    ) throws -> [BlameSpan] {
        let since = blameFloor(tipTime: try repository.commitDetails(target.tip).committerTime)
        let ranges: [BlameRange]
        switch try repository.blame(
            tip: target.tip, path: target.path, limit: target.limit, since: since)
        {
        case let .ranges(found):
            ranges = found
        case let .tooLarge(size, limit):
            throw RepositoryError(
                "cannot blame '\(target.path)': it is \(size) bytes, over the \(limit) byte limit")
        }
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
            spans.append(
                BlameSpan(range: range, commit: commit, olderThan: range.boundary ? since : nil))
        }
        return spans
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
    /// steps back end on the second whichever load finishes first. A restore waits for a ref
    /// refresh under way, and a refresh asked for meanwhile starts once the restore returns.
    public func restore(_ target: BrowserLocation) async {
        guard repository != nil else { return }
        pendingRestore = target
        guard !isRestoring else { return }
        isRestoring = true
        if let refreshRun {
            await refreshRun.value
        }
        beginMove()
        defer {
            isRestoring = false
            endMove()
            // Another move still under way would report where this restore ended once it
            // lands, after the restore returned, and that report would read as a new stop.
            if moveDepth > 0 {
                reportedLocation = location
            }
            if refreshPending {
                Task { await self.refreshRefs() }
            }
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
                opened = await openPath(path) == .opened
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
        pendingFragment = nil
        openedFilePath = nil
        openedFileSource = nil
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

    /// Syntax spans for a text file, computed in the background and cached by blob id and
    /// extension, so a file parses once however often it is reopened. `nil` for unknown
    /// languages, files whose bytes did not decode cleanly (offsets would misalign), or
    /// oversized files.
    public func highlights(for entry: TreeEntry, document: TextDocument) async
        -> [HighlightSpan]?
    {
        guard !document.isLossy, document.byteCount <= Highlighter.byteLimit else { return nil }
        let cacheable = !entry.oid.isUnhashed
        let key = HighlightKey(entry)
        if cacheable, let cached = highlightCache[key] {
            if let index = highlightCacheOrder.firstIndex(of: key) {
                highlightCacheOrder.remove(at: index)
                highlightCacheOrder.append(key)
            }
            return cached
        }
        let name = entry.name
        let text = document.text
        let spans = await Task.detached(priority: .utility) {
            Highlighter.highlight(name: name, data: Array(text.utf8))
        }.value
        if cacheable, let spans {
            storeHighlights(spans, for: key)
        }
        return spans
    }

    /// Inserts one entry and evicts from the least recently used end until both caps hold.
    private func storeHighlights(_ spans: [HighlightSpan], for key: HighlightKey) {
        if highlightCache[key] == nil {
            highlightCacheOrder.append(key)
            highlightCacheSpanCount += spans.count
            highlightCache[key] = spans
        }
        while highlightCacheOrder.count > highlightCacheLimit
            || highlightCacheSpanCount > highlightCacheSpanLimit
        {
            guard highlightCacheOrder.count > 1 else { break }
            let evicted = highlightCacheOrder.removeFirst()
            highlightCacheSpanCount -= highlightCache.removeValue(forKey: evicted)?.count ?? 0
            importLinkCache.removeValue(forKey: evicted)
        }
    }

    // MARK: Import links

    /// The import links of a text file, found in the background. Cached by blob id and
    /// extension for as long as the entry's highlight spans are, so going back and forth
    /// between two files parses neither again. `nil` under the same conditions as `highlights`.
    public func importLinks(for entry: TreeEntry, document: TextDocument) async -> [ImportLink]? {
        guard !document.isLossy, document.byteCount <= Highlighter.byteLimit else { return nil }
        let cacheable = !entry.oid.isUnhashed
        let key = HighlightKey(entry)
        if cacheable, let cached = importLinkCache[key] { return cached }
        let name = entry.name
        let text = document.text
        let links = await Task.detached(priority: .utility) {
            ImportLinks.find(name: name, text: text)
        }.value
        if cacheable, let links, highlightCache[key] != nil {
            importLinkCache[key] = links
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
        return await openPath(path) == .opened
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
    /// Hidden files match only while `showsHiddenFiles` is on. The result carries the index's
    /// `truncated` flag, so the panel can say matches may be missing.
    public func searchPaths(_ query: String, limit: Int = 100) async throws -> PathSearchResult {
        guard let root = rootTree else { return PathSearchResult(entries: [], truncated: false) }
        let index =
            browsingWorkingTree
            ? try await worktreePathIndex(comparedTo: root) : try await pathIndex(for: root)
        let includeHidden = showsHiddenFiles
        let entries = await Task.detached(priority: .userInitiated) {
            index.search(query, limit: limit, includeHidden: includeHidden)
        }.value
        return PathSearchResult(entries: entries, truncated: index.truncated)
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
            await openWorktreeFile(
                name: entry.name, path: entry.path, pathIsExact: entry.hasExactPath)
        } else {
            let treeEntry = TreeEntry(name: entry.name, oid: entry.oid, kind: entry.kind)
            await openFile(treeEntry, path: entry.path, pathIsExact: entry.hasExactPath)
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
        onError?(message)
    }
}

/// A highlight cache key: the blob plus the part of its name the core picks a language from,
/// the lowercased text after the last `.`. Identical bytes under `LICENSE.md` and `LICENSE`
/// share an oid but not a grammar, so the oid alone would hand one the other's colors.
private struct HighlightKey: Hashable {
    let oid: Oid
    let ext: String

    init(_ entry: TreeEntry) {
        oid = entry.oid
        if let dot = entry.name.lastIndex(of: ".") {
            ext = entry.name[entry.name.index(after: dot)...].lowercased()
        } else {
            ext = ""
        }
    }
}
