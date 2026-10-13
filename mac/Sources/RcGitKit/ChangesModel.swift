import Foundation

/// The change list's load state.
public enum ChangesState: Sendable, Hashable {
    case idle
    case loading
    case loaded(StatusReport)
    case failed(String)
}

/// The worktree diff pane's load state, keyed by the path it shows.
public enum WorktreeDiffState: Sendable, Hashable {
    case idle
    case loading(String)
    case loaded(String, BlobDiff)
    case failed(String, String)
}

/// State for the Changes tab: the flat change list, one file's diff against `HEAD`, the
/// stash list, and the tick state that becomes a commit or stash selection. Reads only —
/// guarded actions run in the window controller, which calls `refresh()` afterwards.
///
/// Files are ticked by default; unticks survive a refresh by path. Partial hunk ticks survive
/// one too, but only the stamps that still match a hunk of the file's fresh diff: a stamp that
/// matches nothing would make the guard refuse every later commit.
/// Working-tree data is never cached: disk is the one thing content addressing cannot cover.
///
/// Untracked files `HiddenFiles` names are left out of the list while hidden files are hidden,
/// so they are never part of a commit or stash selection the user cannot see.
@MainActor
public final class ChangesModel {
    private let repository: RepositoryReading
    /// Whether untracked hidden files are listed. Takes effect on the next scan.
    public var showsHiddenFiles: Bool

    public private(set) var statusState: ChangesState = .idle
    public private(set) var diffState: WorktreeDiffState = .idle
    public private(set) var stashes: [StashEntry] = []
    public private(set) var selectedPath: String?
    /// The commit `HEAD` pointed at when the list was loaded; `nil` on an unborn branch.
    /// Guarded commits pass it back so a moved `HEAD` is refused instead of raced.
    public private(set) var expectedHead: Oid?

    private var untickedPaths: Set<String> = []
    /// Paths some status entry carries with U+FFFD replacements. The core reads a path
    /// argument as UTF-8, so such a path would act on a twin with the valid spelling, and the
    /// twin shares the string: every entry with one of these paths stays out of commit, stash
    /// and discard.
    private var inexactPaths: Set<String> = []
    private var tickedHunks: [String: Set<HunkStamp>] = [:]

    private var statusGeneration = 0
    private var diffGeneration = 0
    private var stashGeneration = 0

    /// Whether a scan is running, and whether another was asked for while it ran. A scan is
    /// never started beside one already in flight: the request is merged into a single re-run
    /// that happens once the current scan lands. Both are only ever touched from the main
    /// actor between suspension points, so the flag a finishing scan reads is the flag every
    /// request that arrived during it has already set.
    private var statusScanning = false
    private var statusPending = false
    private var stashScanning = false
    private var stashPending = false

    public var onStatusChanged: (() -> Void)?
    public var onDiffChanged: (() -> Void)?
    public var onStashChanged: (() -> Void)?
    public var onError: ((String) -> Void)?

    public init(repository: RepositoryReading, showsHiddenFiles: Bool = false) {
        self.repository = repository
        self.showsHiddenFiles = showsHiddenFiles
    }

    /// Reloads the change list, the stash list, and the captured `HEAD`.
    public func refresh() async {
        async let status: Void = loadStatus()
        async let stash: Void = loadStashes()
        _ = await (status, stash)
    }

    /// Reloads only the change list, capturing the `HEAD` it was built against.
    ///
    /// Scans are coalesced. A call made while one is running does not start a second: it marks
    /// the running scan's result stale and returns, and the running scan re-reads the working
    /// tree once when it lands. A watcher firing faster than the tree can be walked therefore
    /// costs one scan in flight plus one queued, never one per event — queuing per event is
    /// what let a busy tree accumulate a backlog that never drained.
    ///
    /// Because the request is merged rather than queued, returning does not mean the scan it
    /// asked for has finished; it means the model knows to do it.
    public func loadStatus() async {
        statusPending = true
        guard !statusScanning else { return }
        statusScanning = true
        while statusPending {
            statusPending = false
            await runStatusScan()
        }
        statusScanning = false
    }

    /// Cancels the queued re-scan and drops whatever is in flight, leaving the list idle so
    /// the next switch back to the Changes tab reloads it — a rescan's previous report is
    /// dropped too, since the scan that would have replaced it never lands. The repository
    /// call already handed to the walk runs to completion — it is a synchronous FFI call and
    /// cannot be interrupted — but its result is dropped by the generation check.
    public func cancelScans() {
        statusPending = false
        stashPending = false
        statusGeneration += 1
        stashGeneration += 1
        if statusScanning, statusState != .idle {
            statusState = .idle
            onStatusChanged?()
        }
    }

    /// A rescan keeps the last loaded report in `statusState` until the new one lands, so the
    /// list and the commit and stash selections stay what the user sees. Acting on it is safe:
    /// the guard re-checks `expectedHead` and every hunk stamp.
    private func runStatusScan() async {
        statusGeneration += 1
        let generation = statusGeneration
        let hasReport = if case .loaded = statusState { true } else { false }
        if !hasReport {
            statusState = .loading
            onStatusChanged?()
        }
        let repository = repository
        let showsHidden = showsHiddenFiles
        let partialPaths = Set(tickedHunks.keys)
        let result = await RepositoryError.run {
            // One HEAD read feeds both the tree and the expected head, so they always agree.
            let target = try repository.head().target
            let tree = try target.map { try repository.treeOf($0) }
            let report = Self.listed(
                try repository.worktreeStatus(
                    tree: tree, maxScan: Repository.defaultStatusScanCap),
                showsHidden: showsHidden)
            let hunks = Self.currentHunks(of: partialPaths, in: report, repository: repository)
            return (report, target, hunks)
        }
        guard generation == statusGeneration else { return }
        switch result {
        case let .success((report, head, hunks)):
            statusState = .loaded(report)
            expectedHead = head
            inexactPaths = Set(report.entries.filter { !$0.hasExactPath }.map(\.path))
            let current = Set(report.entries.map(\.path))
            untickedPaths.formIntersection(current)
            tickedHunks = tickedHunks.filter { current.contains($0.key) }
            revalidateHunkTicks(hunks)
            if let selectedPath, !current.contains(selectedPath) {
                self.selectedPath = nil
                diffState = .idle
                onDiffChanged?()
            }
        case let .failure(error):
            statusState = .failed(error.message)
        }
        onStatusChanged?()
    }

    /// Each partially ticked path's current hunk stamps, read from the fresh report's entry:
    /// one diff per partially ticked file, so the cost is bounded by what the user picked,
    /// never by the repository. A file whose diff cannot be read, or is not text, maps to its
    /// read error or to no stamps, and either way its partial tick is dropped.
    nonisolated private static func currentHunks(
        of paths: Set<String>, in report: StatusReport, repository: RepositoryReading
    ) -> [String: Result<Set<HunkStamp>, RepositoryError>] {
        var hunks: [String: Result<Set<HunkStamp>, RepositoryError>] = [:]
        for entry in report.entries where paths.contains(entry.path) {
            hunks[entry.path] = Result {
                guard case let .text(fileHunks) = try worktreeDiff(entry, repository: repository)
                else { return [] }
                return Set(
                    fileHunks.map {
                        HunkStamp(
                            oldStart: $0.oldStart, oldLines: $0.oldLines,
                            newStart: $0.newStart, newLines: $0.newLines)
                    })
            }.mapError { RepositoryError(wrapping: $0) }
        }
        return hunks
    }

    /// Keeps only the ticked stamps that still name a current hunk. Ticking the rest of what
    /// is left collapses to a whole-file tick; nothing left unticks the file rather than
    /// widening the tick to hunks the user never picked. A failed read drops the tick the same
    /// way and is reported.
    private func revalidateHunkTicks(_ hunks: [String: Result<Set<HunkStamp>, RepositoryError>]) {
        for (path, read) in hunks {
            guard let ticked = tickedHunks[path] else { continue }
            let current: Set<HunkStamp>
            switch read {
            case let .success(stamps):
                current = stamps
            case let .failure(error):
                current = []
                onError?(error.message)
            }
            let kept = ticked.intersection(current)
            if kept.isEmpty {
                untickedPaths.insert(path)
                tickedHunks[path] = nil
            } else if kept == current {
                tickedHunks[path] = nil
            } else {
                tickedHunks[path] = kept
            }
        }
    }

    /// Reloads the stash list. Coalesced the same way as `loadStatus()`, and with the same
    /// caveat: returning means the reload is accounted for, not that it has happened.
    public func loadStashes() async {
        stashPending = true
        guard !stashScanning else { return }
        stashScanning = true
        while stashPending {
            stashPending = false
            await runStashScan()
        }
        stashScanning = false
    }

    private func runStashScan() async {
        stashGeneration += 1
        let generation = stashGeneration
        let repository = repository
        let result = await RepositoryError.run {
            try repository.stashList()
        }
        guard generation == stashGeneration else { return }
        switch result {
        case let .success(entries):
            stashes = entries
            onStashChanged?()
        case let .failure(error):
            onError?(error.message)
        }
    }

    /// Shows `entry`'s diff against `HEAD` in the detail pane. A file on disk whose path is
    /// not exact is not read, since the read goes by path.
    public func select(_ entry: StatusEntry) async {
        selectedPath = entry.path
        diffGeneration += 1
        let generation = diffGeneration
        diffState = .loading(entry.path)
        onDiffChanged?()
        let repository = repository
        let result = await RepositoryError.run {
            try Self.worktreeDiff(entry, repository: repository)
        }
        guard generation == diffGeneration, selectedPath == entry.path else { return }
        switch result {
        case let .success(diff):
            diffState = .loaded(entry.path, diff)
        case let .failure(error):
            diffState = .failed(entry.path, error.message)
        }
        onDiffChanged?()
    }

    /// `entry`'s diff against `HEAD`: the old blob alone for a deleted file, else the file on
    /// disk, which is refused when its path is not exact.
    nonisolated private static func worktreeDiff(
        _ entry: StatusEntry, repository: RepositoryReading
    ) throws -> BlobDiff {
        if entry.status == .deleted {
            return try repository.diffBlobs(
                old: entry.oid, new: nil, limit: Repository.defaultBlobLimit, context: 3)
        }
        guard entry.hasExactPath else {
            throw RepositoryError(InexactName.pathRefusal("diff", entry.path))
        }
        return try repository.diffWorktreeFile(
            path: entry.path, old: entry.oid, limit: Repository.defaultBlobLimit, context: 3)
    }

    // MARK: - Tick state

    /// Whether `path` may be committed, stashed or discarded: `false` when a status entry
    /// carries it with U+FFFD replacements.
    public func isActionable(_ path: String) -> Bool {
        !inexactPaths.contains(path)
    }

    /// The refusal to show for discarding `paths`, or `nil` when every one is actionable.
    public func discardRefusal(_ paths: [String]) -> String? {
        guard let path = paths.first(where: { !isActionable($0) }) else { return nil }
        return InexactName.pathRefusal("discard", path)
    }

    /// Whether the whole file is in the selection. Partially ticked files count as ticked;
    /// a path that is not actionable never is.
    public func isTicked(_ path: String) -> Bool {
        isActionable(path) && !untickedPaths.contains(path)
    }

    /// Whether only some hunks of `path` are selected.
    public func isPartiallyTicked(_ path: String) -> Bool {
        isTicked(path) && tickedHunks[path] != nil
    }

    /// Ticks or unticks whole files. A folder row in the Changes list hands over every path
    /// beneath it, so one click moves them together and renders once.
    public func setTicked(_ paths: [String], _ ticked: Bool) {
        for path in paths {
            if ticked {
                untickedPaths.remove(path)
            } else {
                untickedPaths.insert(path)
            }
            tickedHunks[path] = nil
        }
        onStatusChanged?()
    }

    /// Whether one hunk of `path` is in the selection. With no partial state, every hunk of
    /// a ticked file is.
    public func isHunkTicked(_ path: String, _ stamp: HunkStamp) -> Bool {
        guard isTicked(path) else { return false }
        guard let stamps = tickedHunks[path] else { return true }
        return stamps.contains(stamp)
    }

    /// Toggles one hunk. `all` is the file's current full hunk list, so ticking the last
    /// missing hunk collapses back to a whole-file tick and unticking the last one unticks
    /// the file.
    public func toggleHunk(_ path: String, _ stamp: HunkStamp, all: [HunkStamp]) {
        guard isActionable(path) else { return }
        var stamps = tickedHunks[path] ?? (isTicked(path) ? Set(all) : [])
        if stamps.contains(stamp) {
            stamps.remove(stamp)
        } else {
            stamps.insert(stamp)
        }
        untickedPaths.remove(path)
        if stamps.isEmpty {
            untickedPaths.insert(path)
            tickedHunks[path] = nil
        } else if stamps == Set(all) {
            tickedHunks[path] = nil
        } else {
            tickedHunks[path] = stamps
        }
        onStatusChanged?()
    }

    /// The ticked paths, for stash and discard.
    public var tickedPaths: [String] {
        guard case let .loaded(report) = statusState else { return [] }
        return report.entries.map(\.path).filter { isTicked($0) }
    }

    /// The commit selection: whole files, except where only some hunks are ticked.
    public var selections: [CommitSelection] {
        guard case let .loaded(report) = statusState else { return [] }
        var selections: [CommitSelection] = []
        for entry in report.entries where isTicked(entry.path) {
            if let stamps = tickedHunks[entry.path] {
                selections.append(
                    .hunks(
                        path: entry.path,
                        stamps: stamps.sorted { $0.oldStart < $1.oldStart }))
            } else {
                selections.append(.whole(path: entry.path))
            }
        }
        return selections
    }

    /// The report as the list shows it: without untracked hidden files unless `showsHidden`.
    /// A tracked hidden file that changed stays, since hiding it would hide a real edit.
    nonisolated static func listed(_ report: StatusReport, showsHidden: Bool) -> StatusReport {
        guard !showsHidden else { return report }
        let entries = report.entries.filter {
            $0.status != .untracked || !HiddenFiles.isHidden(path: $0.path)
        }
        return StatusReport(entries: entries, scanned: report.scanned, truncated: report.truncated)
    }
}
