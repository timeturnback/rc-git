import Foundation
import os

/// The change list's load state.
public enum ChangesState: Sendable, Hashable {
    case idle
    case loading
    case loaded(StatusReport)
    case failed(String)
}

/// What the change list shows besides its rows: the line in place of them, and whether the
/// rows on screen are the state's own.
public struct ChangesListNotice: Sendable, Hashable {
    /// The line shown over the list; `nil` when the rows stand alone.
    public let placeholder: String?
    /// Whether the rows on screen came from the current state; `false` while they are left
    /// over from a report a scan that has not landed yet will replace.
    public let isCurrent: Bool

    public init(placeholder: String?, isCurrent: Bool) {
        self.placeholder = placeholder
        self.isCurrent = isCurrent
    }
}

extension ChangesState {
    /// What the list shows while no scan has landed: the walk can take seconds on a large
    /// checkout, and "No changes" in that time reads as a clean tree.
    public static let scanningPlaceholder = "Scanning the working tree…"
    public static let cleanPlaceholder = "No changes"

    /// The rows this state replaces the list with, or `nil` when it carries none and the rows
    /// on screen stay as they are. Only `nil` spares the view a rebuild, which on a large
    /// change list is the costliest step of rendering a status change.
    public var listedEntries: [StatusEntry]? {
        switch self {
        case .idle, .loading: nil
        case let .loaded(report): report.entries
        case .failed: []
        }
    }

    /// The list's notice in this state. Only a loaded report with no entries says the tree
    /// is clean; before one lands, the list says it is scanning, and rows left from an earlier
    /// report stay but are not current.
    ///
    /// - Parameter showingRows: Whether the list still holds rows from an earlier report.
    public func notice(showingRows: Bool) -> ChangesListNotice {
        switch self {
        case .idle, .loading:
            ChangesListNotice(
                placeholder: showingRows ? nil : Self.scanningPlaceholder, isCurrent: false)
        case let .loaded(report):
            ChangesListNotice(
                placeholder: report.entries.isEmpty ? Self.cleanPlaceholder : nil,
                isCurrent: true)
        case let .failed(message):
            ChangesListNotice(placeholder: message, isCurrent: true)
        }
    }
}

/// The worktree diff pane's load state, keyed by the path it shows.
public enum WorktreeDiffState: Sendable, Hashable {
    case idle
    case loading(String)
    case loaded(String, BlobDiff)
    case failed(String, String)
}

/// The stash list's load state. A failed read keeps the last list in `stashes`, so the state
/// is what tells the view that list is no longer current.
public enum StashListState: Sendable, Hashable {
    case loaded
    case failed(String)
}

/// A change list row's tick box. A file with only some hunks ticked is `mixed`, like a folder
/// with only some files ticked: the row never claims more than the commit will take.
public enum TickState: Sendable, Hashable {
    case off
    case mixed
    case on
}

/// What a change list row's tick box shows, and whether it takes a click: a row whose every
/// path is inexact has nothing it could tick.
public struct RowTick: Sendable, Hashable {
    public let state: TickState
    public let isEnabled: Bool

    public init(state: TickState, isEnabled: Bool) {
        self.state = state
        self.isEnabled = isEnabled
    }
}

/// The line under the change list and whether Commit and Stash have anything to take.
public struct TickSummary: Sendable, Hashable {
    /// "2 of 3 selected", or empty when the list is.
    public let text: String
    public let hasSelection: Bool

    public init(text: String, hasSelection: Bool) {
        self.text = text
        self.hasSelection = hasSelection
    }
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
    private nonisolated static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "rcgit", category: "changes")

    private let repository: RepositoryReading
    /// Whether untracked hidden files are listed. Takes effect on the next scan.
    public var showsHiddenFiles: Bool

    public private(set) var statusState: ChangesState = .idle {
        didSet { cachedTickCounts = nil }
    }
    public private(set) var diffState: WorktreeDiffState = .idle
    public private(set) var stashes: [StashEntry] = []
    public private(set) var stashState: StashListState = .loaded
    public private(set) var selectedPath: String?
    /// The commit `HEAD` pointed at when the list was loaded; `nil` on an unborn branch.
    /// Guarded commits pass it back so a moved `HEAD` is refused instead of raced.
    public private(set) var expectedHead: Oid?

    private var untickedPaths: Set<String> = [] {
        didSet { cachedTickCounts = nil }
    }
    /// Paths some status entry carries with U+FFFD replacements. The core reads a path
    /// argument as UTF-8, so such a path would act on a twin with the valid spelling, and the
    /// twin shares the string: every entry with one of these paths stays out of commit, stash
    /// and discard.
    private var inexactPaths: Set<String> = [] {
        didSet { cachedTickCounts = nil }
    }
    private var tickedHunks: [String: Set<HunkStamp>] = [:] {
        didSet { cachedTickCounts = nil }
    }
    /// Tick counts per folder path, built once per change to the tick state or the list, so
    /// rendering a folder row reads one entry instead of walking every path beneath it.
    private var cachedTickCounts: TickCounts?

    private var statusGeneration = 0
    private var diffGeneration = 0
    private var stashGeneration = 0
    /// The stash failure last sent to `onError`. A watcher refresh re-reads the stash list on
    /// every event, so a failure that persists would otherwise raise one alert per event.
    private var reportedStashFailure: String?

    /// Whether a scan is running, and whether another was asked for while it ran. A scan is
    /// never started beside one already in flight: the request is merged into a single re-run
    /// that happens once the current scan lands. Both are only ever touched from the main
    /// actor between suspension points, so the flag a finishing scan reads is the flag every
    /// request that arrived during it has already set.
    private var statusScanning = false
    private var statusPending = false
    private var stashScanning = false
    private var stashPending = false
    /// Whether the list is on screen, as the window last said through `setVisible`, and
    /// whether the disk moved while it was not. A hidden list scans nothing: it remembers it
    /// is stale and pays one rescan when it is shown again.
    public private(set) var isVisible = false
    public private(set) var isStale = false

    /// Fires when a scan moves `statusState`: the list itself may have changed.
    public var onStatusChanged: (() -> Void)?
    /// Fires when only the tick state moved: the list is the same, so the view redraws its
    /// tick boxes and summary without rebuilding the rows.
    public var onTicksChanged: (() -> Void)?
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

    /// The working tree, the refs or the hidden-files setting changed. A visible list
    /// rescans now; a hidden one only remembers, so a burst of events while it is off screen
    /// costs no scan at all.
    public func markStale() async {
        guard isVisible else {
            isStale = true
            return
        }
        await refresh()
    }

    /// Shows or hides the list. Showing it rescans when it went stale while hidden or has
    /// never loaded; hiding it cancels the running scan, which is work for no one.
    public func setVisible(_ visible: Bool) async {
        isVisible = visible
        guard visible else {
            cancelScans()
            return
        }
        let needsLoad = isStale || statusState == .idle
        isStale = false
        if needsLoad {
            await refresh()
        }
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
        var selectedEntry: StatusEntry?
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
            selectedEntry = report.entries.first { $0.path == selectedPath }
        case let .failure(error):
            statusState = .failed(error.message)
        }
        onStatusChanged?()
        if let selectedEntry {
            await reloadDiff(selectedEntry)
        }
    }

    /// Re-reads the selected file's diff from a fresh scan's entry, whose `oid` follows a moved
    /// `HEAD`. Unlike `select(_:)` it never shows `.loading`, so a watcher-driven rescan does not
    /// flash the placeholder, and it fires `onDiffChanged` only when what the pane shows
    /// changed, so an unchanged file keeps its table and scroll.
    private func reloadDiff(_ entry: StatusEntry) async {
        diffGeneration += 1
        let generation = diffGeneration
        let fresh = await readDiffState(entry)
        guard generation == diffGeneration, selectedPath == entry.path, fresh != diffState
        else { return }
        diffState = fresh
        onDiffChanged?()
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
                return Set(fileHunks.map(\.stamp))
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
                report(error.message)
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

    /// A failure is shown through `stashState` every time and sent to `onError` once per
    /// distinct message, until a read succeeds again.
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
            stashState = .loaded
            reportedStashFailure = nil
        case let .failure(error):
            stashState = .failed(error.message)
            if reportedStashFailure != error.message {
                reportedStashFailure = error.message
                report(error.message)
            }
        }
        onStashChanged?()
    }

    /// Logs a failure, then shows it through `onError`.
    private func report(_ message: String) {
        Self.logger.error("\(message, privacy: .public)")
        onError?(message)
    }

    /// Shows `entry`'s diff against `HEAD` in the detail pane. A file on disk whose path is
    /// not exact is not read, since the read goes by path.
    public func select(_ entry: StatusEntry) async {
        selectedPath = entry.path
        diffGeneration += 1
        let generation = diffGeneration
        diffState = .loading(entry.path)
        onDiffChanged?()
        let fresh = await readDiffState(entry)
        guard generation == diffGeneration, selectedPath == entry.path else { return }
        diffState = fresh
        onDiffChanged?()
    }

    /// `entry`'s diff read off the main actor, as the state the pane shows for it.
    private func readDiffState(_ entry: StatusEntry) async -> WorktreeDiffState {
        let repository = repository
        let result = await RepositoryError.run {
            try Self.worktreeDiff(entry, repository: repository)
        }
        return switch result {
        case let .success(diff): .loaded(entry.path, diff)
        case let .failure(error): .failed(entry.path, error.message)
        }
    }

    /// `entry`'s diff against `HEAD`: the old blob alone for a deleted file, else the file on
    /// disk, which is refused when its path is not exact.
    nonisolated private static func worktreeDiff(
        _ entry: StatusEntry, repository: RepositoryReading
    ) throws -> BlobDiff {
        if entry.status == .deleted {
            return try repository.diffBlobs(
                old: entry.oid, new: nil, limit: Repository.defaultBlobLimit,
                context: Repository.defaultDiffContext)
        }
        guard entry.hasExactPath else {
            throw RepositoryError(InexactName.pathRefusal("diff", entry.path))
        }
        return try repository.diffWorktreeFile(
            path: entry.path, old: entry.oid, limit: Repository.defaultBlobLimit,
            context: Repository.defaultDiffContext)
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
        onTicksChanged?()
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
        onTicksChanged?()
    }

    /// The tick box of one change list row: a file's own state, or the sum of every file
    /// beneath a folder.
    public func rowTick(of node: ChangeNode<StatusEntry>) -> RowTick {
        let count =
            node.isFolder ? tickCounts.folders[node.path] ?? TickCount() : tickCount(node.path)
        return RowTick(state: count.state, isEnabled: count.actionable > 0)
    }

    /// How many files are in the selection, partially ticked ones included.
    public var tickSummary: TickSummary {
        let all = tickCounts.all
        return TickSummary(
            text: all.total == 0 ? "" : "\(all.ticked) of \(all.total) selected",
            hasSelection: all.ticked > 0)
    }

    private var tickCounts: TickCounts {
        if let cachedTickCounts { return cachedTickCounts }
        var counts = TickCounts()
        if case let .loaded(report) = statusState {
            for entry in report.entries {
                let one = tickCount(entry.path)
                counts.all.add(one)
                let path = entry.path
                for index in path.utf8.indices where path.utf8[index] == UInt8(ascii: "/") {
                    counts.folders[String(path[..<index]), default: TickCount()].add(one)
                }
            }
        }
        cachedTickCounts = counts
        return counts
    }

    private func tickCount(_ path: String) -> TickCount {
        let ticked = isTicked(path)
        return TickCount(
            total: 1, ticked: ticked ? 1 : 0,
            whole: ticked && !isPartiallyTicked(path) ? 1 : 0,
            actionable: isActionable(path) ? 1 : 0)
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

/// Files at or below one row: how many there are, how many are in the selection at all, how
/// many wholly, and how many could be.
private struct TickCount {
    var total = 0
    var ticked = 0
    var whole = 0
    var actionable = 0

    var state: TickState {
        if ticked == 0 { return .off }
        return whole == total ? .on : .mixed
    }

    mutating func add(_ other: TickCount) {
        total += other.total
        ticked += other.ticked
        whole += other.whole
        actionable += other.actionable
    }
}

/// Every folder path's tick count, and the whole list's.
private struct TickCounts {
    var folders: [String: TickCount] = [:]
    var all = TickCount()
}
