import Foundation
import Testing

@testable import RcGitKit

@MainActor
@Suite(.timeLimit(.minutes(1))) struct ChangesModelTests {
    let repo = FakeRepository()
    let model: ChangesModel
    let head = oid(0xAA)
    let tree = oid(0xBB)

    init() {
        repo.headValue = Head(refName: "refs/heads/main", target: head)
        repo.treeOfMap[head] = tree
        repo.statusReport = StatusReport(
            entries: [
                StatusEntry(
                    path: "a.txt", kind: .blob, status: .modified, oid: oid(0x01), size: 3),
                StatusEntry(path: "b.txt", kind: .blob, status: .untracked, oid: nil, size: 2),
                StatusEntry(
                    path: "gone.txt", kind: .blob, status: .deleted, oid: oid(0x02), size: 0),
            ], scanned: 5, truncated: false)
        model = ChangesModel(repository: repo)
    }

    @Test func inexactNameStaysOutOfCommitStashAndDiscard() async {
        let spelling = "f\u{FFFD}o"
        repo.statusReport = StatusReport(
            entries: [
                StatusEntry(
                    path: "a.txt", kind: .blob, status: .modified, oid: oid(0x01), size: 3),
                StatusEntry(
                    path: spelling, kind: .blob, status: .deleted, oid: oid(0x02), size: 0,
                    hasExactPath: false),
                StatusEntry(
                    path: spelling, kind: .blob, status: .untracked, oid: nil, size: 1),
            ], scanned: 4, truncated: false)
        await model.loadStatus()
        #expect(!model.isActionable(spelling), "the valid twin shares the spelling")
        #expect(!model.isTicked(spelling))
        model.setTicked([spelling], true)
        let stamp = HunkStamp(oldStart: 1, oldLines: 1, newStart: 1, newLines: 1)
        model.toggleHunk(spelling, stamp, all: [stamp])
        #expect(model.tickedPaths == ["a.txt"])
        #expect(model.selections == [.whole(path: "a.txt")])
        #expect(model.discardRefusal(["a.txt"]) == nil)
        #expect(
            model.discardRefusal(["a.txt", spelling])
                == "cannot discard 'f\u{FFFD}o': the path is not valid UTF-8")
    }

    @Test func loadsStatusAndCapturesExpectedHead() async {
        var statusCallbacks = 0
        model.onStatusChanged = { statusCallbacks += 1 }
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        #expect(report.entries.count == 3)
        #expect(model.expectedHead == head)
        #expect(repo.lock.withLock { repo.statusTrees } == [tree])
        #expect(statusCallbacks == 2, "one for loading, one for loaded")
    }

    @Test func anUnbornBranchListsEveryFileAsUntracked() async {
        repo.headValue = Head(refName: "refs/heads/main", target: nil)
        repo.statusReport = StatusReport(
            entries: [
                StatusEntry(path: "a.txt", kind: .blob, status: .untracked, oid: nil, size: 3),
                StatusEntry(path: "src/b.txt", kind: .blob, status: .untracked, oid: nil, size: 2),
            ], scanned: 3, truncated: false)
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        #expect(report.entries.map(\.path) == ["a.txt", "src/b.txt"])
        #expect(report.entries.allSatisfy { $0.status == .untracked })
        #expect(model.expectedHead == nil)
        #expect(repo.lock.withLock { repo.statusTrees } == [nil], "scanned with no tree")
        #expect(model.tickedPaths == ["a.txt", "src/b.txt"])
    }

    @Test func untrackedHiddenFilesStayOutOfTheListAndTheSelection() async {
        repo.statusReport = StatusReport(
            entries: [
                StatusEntry(path: ".DS_Store", kind: .blob, status: .untracked, oid: nil, size: 6),
                StatusEntry(
                    path: "assets/.DS_Store", kind: .blob, status: .modified, oid: oid(0x03),
                    size: 6),
                StatusEntry(path: "b.txt", kind: .blob, status: .untracked, oid: nil, size: 2),
                StatusEntry(
                    path: "sub/._b.txt", kind: .blob, status: .untracked, oid: nil, size: 2),
            ], scanned: 9, truncated: true)
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        #expect(
            report.entries.map(\.path) == ["assets/.DS_Store", "b.txt"],
            "a tracked hidden file that changed stays listed")
        #expect((report.scanned, report.truncated) == (9, true))
        #expect(model.tickedPaths == ["assets/.DS_Store", "b.txt"])

        model.showsHiddenFiles = true
        await model.loadStatus()
        #expect(model.tickedPaths == [".DS_Store", "assets/.DS_Store", "b.txt", "sub/._b.txt"])
    }

    @Test func aFailedScanShowsTheError() async {
        repo.failNextStatus = true
        await model.loadStatus()
        guard case let .failed(message) = model.statusState else {
            Issue.record("expected failed, got \(model.statusState)")
            return
        }
        #expect(message.contains("scan working tree"))
    }

    @Test func aBurstOfRequestsCostsOneRescan() async {
        let gate = CallGate()
        repo.statusGate = gate
        async let inFlight: Void = model.loadStatus()
        await gate.waitUntilArrived()
        for _ in 0..<20 {
            await model.loadStatus()
        }
        gate.open()
        await inFlight
        #expect(
            repo.lock.withLock { repo.statusCalls } == 2,
            "the scan in flight plus one re-run, however many requests arrived")
        guard case .loaded = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
    }

    @Test func aCancelledScanIsDroppedAndLeavesTheListIdle() async {
        let gate = CallGate()
        repo.statusGate = gate
        async let inFlight: Void = model.loadStatus()
        await gate.waitUntilArrived()
        model.cancelScans()
        #expect(model.statusState == .idle, "the tab reloads on the next switch back")
        gate.open()
        await inFlight
        #expect(model.statusState == .idle, "the cancelled scan's result never lands")
        #expect(
            repo.lock.withLock { repo.statusCalls } == 1, "a cancel drops the queued re-run")
    }

    @Test func theSelectionSurvivesARescanInFlight() async {
        await model.loadStatus()
        let before = model.statusState
        let gate = CallGate()
        repo.statusGate = gate
        async let rescan: Void = model.loadStatus()
        await gate.waitUntilArrived()
        #expect(model.statusState == before, "the list keeps the last report while it rescans")
        #expect(
            model.selections == [
                .whole(path: "a.txt"), .whole(path: "b.txt"),
                .whole(path: "gone.txt"),
            ])
        #expect(model.tickedPaths == ["a.txt", "b.txt", "gone.txt"])
        gate.open()
        await rescan
    }

    @Test func aCancelledRescanDropsTheOldReport() async {
        await model.loadStatus()
        let gate = CallGate()
        repo.statusGate = gate
        async let rescan: Void = model.loadStatus()
        await gate.waitUntilArrived()
        model.cancelScans()
        #expect(model.statusState == .idle, "the tab reloads on the next switch back")
        gate.open()
        await rescan
        #expect(model.statusState == .idle, "the cancelled scan's result never lands")
    }

    @Test func staleMarksWhileHiddenCostNoScanAndOneOnShow() async {
        await model.setVisible(true)
        await model.setVisible(false)
        let before = repo.lock.withLock { (repo.statusCalls, repo.stashCalls) }
        for _ in 0..<20 {
            await model.markStale()
        }
        #expect(repo.lock.withLock { repo.statusCalls } == before.0, "a hidden list scans nothing")
        #expect(repo.lock.withLock { repo.stashCalls } == before.1)
        #expect(model.isStale)

        await model.setVisible(true)
        #expect(repo.lock.withLock { repo.statusCalls } == before.0 + 1, "one rescan on show")
        #expect(repo.lock.withLock { repo.stashCalls } == before.1 + 1)
        #expect(!model.isStale, "showing consumes the stale mark")

        await model.setVisible(true)
        #expect(
            repo.lock.withLock { repo.statusCalls } == before.0 + 1,
            "showing a list that is current does not rescan")
    }

    @Test func aVisibleListRescansOnEveryStaleMark() async {
        await model.setVisible(true)
        await model.markStale()
        await model.markStale()
        #expect(repo.lock.withLock { repo.statusCalls } == 3)
        #expect(!model.isStale)
    }

    @Test func showingANeverLoadedListLoadsIt() async {
        #expect(!model.isStale)
        await model.setVisible(true)
        #expect(repo.lock.withLock { repo.statusCalls } == 1, "an idle list loads on show")
    }

    @Test func hidingCancelsTheScanInFlight() async {
        await model.setVisible(true)
        let gate = CallGate()
        repo.statusGate = gate
        async let rescan: Void = model.markStale()
        await gate.waitUntilArrived()
        await model.setVisible(false)
        #expect(model.statusState == .idle, "the list reloads when shown again")
        gate.open()
        await rescan
        #expect(model.statusState == .idle, "the cancelled scan's result never lands")

        await model.setVisible(true)
        #expect(repo.lock.withLock { repo.statusCalls } == 3, "the idle list reloads on show")
    }

    @Test func filesAreTickedByDefaultAndUnticksSurviveRefresh() async {
        await model.loadStatus()
        #expect(model.isTicked("a.txt"))
        #expect(model.tickedPaths == ["a.txt", "b.txt", "gone.txt"])

        model.setTicked(["b.txt"], false)
        #expect(model.tickedPaths == ["a.txt", "gone.txt"])
        #expect(
            model.selections == [.whole(path: "a.txt"), .whole(path: "gone.txt")])

        await model.loadStatus()
        #expect(!model.isTicked("b.txt"), "the untick survives a refresh")

        // A path that vanished from the report no longer pins state.
        repo.lock.withLock {
            repo.statusReport = StatusReport(
                entries: [
                    StatusEntry(
                        path: "a.txt", kind: .blob, status: .modified, oid: oid(0x01), size: 3)
                ], scanned: 1, truncated: false)
        }
        await model.loadStatus()
        #expect(model.tickedPaths == ["a.txt"])
    }

    @Test func hunkTicksCollapseToWholeFileAndToUnticked() async {
        await model.loadStatus()
        let first = HunkStamp(oldStart: 1, oldLines: 3, newStart: 1, newLines: 3)
        let second = HunkStamp(oldStart: 8, oldLines: 2, newStart: 8, newLines: 3)
        let all = [first, second]

        #expect(model.isHunkTicked("a.txt", first), "every hunk of a ticked file is ticked")
        model.toggleHunk("a.txt", second, all: all)
        #expect(model.isPartiallyTicked("a.txt"))
        #expect(model.isHunkTicked("a.txt", first))
        #expect(!model.isHunkTicked("a.txt", second))
        #expect(
            model.selections.first == .hunks(path: "a.txt", stamps: [first]))

        model.toggleHunk("a.txt", second, all: all)
        #expect(!model.isPartiallyTicked("a.txt"), "all hunks ticked collapses to whole")
        #expect(model.selections.first == .whole(path: "a.txt"))

        model.toggleHunk("a.txt", first, all: all)
        model.toggleHunk("a.txt", second, all: all)
        #expect(!model.isTicked("a.txt"), "no hunks left unticks the file")

        model.toggleHunk("a.txt", first, all: all)
        #expect(model.isTicked("a.txt"), "ticking a hunk of an unticked file re-ticks it")
        #expect(model.selections.first == .hunks(path: "a.txt", stamps: [first]))
    }

    private func folderReport() -> StatusReport {
        let paths = ["src/a.txt", "src/b.txt", "src/deep/c.txt", "top.txt"]
        return StatusReport(
            entries: paths.map {
                StatusEntry(path: $0, kind: .blob, status: .modified, oid: oid(0x01), size: 3)
            }, scanned: 4, truncated: false)
    }

    private func row(_ key: String) throws -> ChangeNode<StatusEntry> {
        guard case let .loaded(report) = model.statusState else {
            throw RepositoryError("not loaded: \(model.statusState)")
        }
        var pending = ChangeTree.nodes(report.entries, grouping: .folders)
        while let node = pending.popLast() {
            if node.key == key { return node }
            pending += node.children
        }
        throw RepositoryError("no row \(key)")
    }

    @Test func rowTicksCountEveryFileBeneathAFolder() async throws {
        repo.statusReport = folderReport()
        await model.loadStatus()
        #expect(try model.rowTick(of: row("src/")) == RowTick(state: .on, isEnabled: true))
        #expect(model.tickSummary == TickSummary(text: "4 of 4 selected", hasSelection: true))

        model.setTicked(["src/deep/c.txt"], false)
        #expect(try model.rowTick(of: row("src/")).state == .mixed, "some files ticked")
        #expect(try model.rowTick(of: row("src/deep/")).state == .off)
        #expect(try model.rowTick(of: row("src/a.txt")).state == .on)
        #expect(model.tickSummary == TickSummary(text: "3 of 4 selected", hasSelection: true))

        model.setTicked(["src/a.txt", "src/b.txt", "top.txt"], false)
        #expect(try model.rowTick(of: row("src/")).state == .off, "no file ticked")
        #expect(model.tickSummary == TickSummary(text: "0 of 4 selected", hasSelection: false))
    }

    @Test func aFileWithSomeHunksTickedIsMixedAndSoIsItsFolder() async throws {
        repo.statusReport = folderReport()
        await model.loadStatus()
        let first = Self.firstHunk.stamp
        let second = Self.secondHunk.stamp
        model.toggleHunk("src/a.txt", second, all: [first, second])
        #expect(try model.rowTick(of: row("src/a.txt")).state == .mixed)
        #expect(try model.rowTick(of: row("src/")).state == .mixed)
        #expect(model.tickSummary.text == "4 of 4 selected", "a partial file is still selected")

        model.toggleHunk("src/a.txt", second, all: [first, second])
        #expect(try model.rowTick(of: row("src/a.txt")).state == .on)
        #expect(try model.rowTick(of: row("src/")).state == .on)
    }

    @Test func aRowOfOnlyInexactPathsTakesNoClick() async throws {
        let spelling = "dir/f\u{FFFD}o"
        repo.statusReport = StatusReport(
            entries: [
                StatusEntry(
                    path: spelling, kind: .blob, status: .deleted, oid: oid(0x02), size: 0,
                    hasExactPath: false),
                StatusEntry(path: "top.txt", kind: .blob, status: .untracked, oid: nil, size: 1),
            ], scanned: 2, truncated: false)
        await model.loadStatus()
        #expect(try model.rowTick(of: row("dir/")) == RowTick(state: .off, isEnabled: false))
        #expect(try model.rowTick(of: row(spelling)) == RowTick(state: .off, isEnabled: false))
        #expect(model.tickSummary == TickSummary(text: "1 of 2 selected", hasSelection: true))
    }

    @Test func anEmptyListHasNoSummaryAndNothingToCommit() async {
        #expect(model.tickSummary == TickSummary(text: "", hasSelection: false), "not loaded")
        repo.statusReport = StatusReport(entries: [], scanned: 0, truncated: false)
        await model.loadStatus()
        #expect(model.tickSummary == TickSummary(text: "", hasSelection: false))
    }

    @Test func aHunkStampCarriesEachHeaderRangeInItsOwnField() {
        let hunk = DiffHunk(
            oldStart: 3, oldLines: 5, newStart: 7, newLines: 11,
            lines: [DiffLine(kind: .added, text: "x")])
        #expect(hunk.stamp == HunkStamp(oldStart: 3, oldLines: 5, newStart: 7, newLines: 11))
    }

    private static let firstHunk = DiffHunk(
        oldStart: 1, oldLines: 3, newStart: 1, newLines: 3,
        lines: [DiffLine(kind: .added, text: "first")])
    private static let secondHunk = DiffHunk(
        oldStart: 8, oldLines: 2, newStart: 8, newLines: 3,
        lines: [DiffLine(kind: .added, text: "second")])

    private func reportWith(aTxt: Bool) -> StatusReport {
        var entries = [
            StatusEntry(path: "b.txt", kind: .blob, status: .untracked, oid: nil, size: 2)
        ]
        if aTxt {
            entries.insert(
                StatusEntry(
                    path: "a.txt", kind: .blob, status: .modified, oid: oid(0x01), size: 3),
                at: 0)
        }
        return StatusReport(entries: entries, scanned: 2, truncated: false)
    }

    @Test func aHunkTickThatNoLongerMatchesIsDroppedOnRefresh() async {
        let first = Self.firstHunk.stamp
        let second = Self.secondHunk.stamp
        await model.loadStatus()
        model.toggleHunk("a.txt", second, all: [first, second])
        #expect(model.selections.first == .hunks(path: "a.txt", stamps: [first]))

        // The ticked hunk was committed; only the unticked one is left on disk.
        repo.lock.withLock { repo.worktreeDiffs["a.txt"] = .text(hunks: [Self.secondHunk]) }
        await model.loadStatus()
        #expect(!model.selections.contains(.hunks(path: "a.txt", stamps: [first])))
        #expect(!model.isTicked("a.txt"), "never widened to hunks the user did not tick")
        #expect(model.selections == [.whole(path: "b.txt"), .whole(path: "gone.txt")])
    }

    @Test func aHunkTickWhoseDiffCannotBeReadIsDroppedAndReported() async {
        let first = Self.firstHunk.stamp
        let second = Self.secondHunk.stamp
        var errors: [String] = []
        model.onError = { errors.append($0) }
        await model.loadStatus()
        model.toggleHunk("a.txt", second, all: [first, second])
        await model.loadStatus()
        #expect(!model.isTicked("a.txt"))
        #expect(errors == ["read working tree file: 'a.txt': missing"])
    }

    @Test func partialHunkTicksSurviveARefreshAndDropWithTheirPath() async {
        let first = Self.firstHunk.stamp
        let second = Self.secondHunk.stamp
        repo.lock.withLock {
            repo.statusReport = reportWith(aTxt: true)
            repo.worktreeDiffs["a.txt"] = .text(hunks: [Self.firstHunk, Self.secondHunk])
        }
        await model.loadStatus()
        model.toggleHunk("a.txt", second, all: [first, second])

        await model.loadStatus()
        #expect(model.isPartiallyTicked("a.txt"))
        #expect(model.selections.first == .hunks(path: "a.txt", stamps: [first]))

        repo.lock.withLock { repo.statusReport = reportWith(aTxt: false) }
        await model.loadStatus()
        repo.lock.withLock { repo.statusReport = reportWith(aTxt: true) }
        await model.loadStatus()
        #expect(!model.isPartiallyTicked("a.txt"))
        #expect(model.selections.first == .whole(path: "a.txt"))
    }

    @Test func aLaterFileSelectionWins() async throws {
        await model.loadStatus()
        let staleHunk = DiffHunk(
            oldStart: 1, oldLines: 1, newStart: 1, newLines: 1,
            lines: [DiffLine(kind: .added, text: "stale")])
        let laterHunk = DiffHunk(
            oldStart: 0, oldLines: 0, newStart: 1, newLines: 1,
            lines: [DiffLine(kind: .added, text: "later")])
        repo.lock.withLock {
            repo.worktreeDiffs["a.txt"] = .text(hunks: [staleHunk])
            repo.worktreeDiffs["b.txt"] = .text(hunks: [laterHunk])
            repo.worktreeDiffDelays["a.txt"] = 1
        }
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        var shown: [WorktreeDiffState] = []
        model.onDiffChanged = { shown.append(self.model.diffState) }
        try #require(report.entries.count == 3)
        async let slowSelect: Void = model.select(report.entries[0])
        try? await Task.sleep(for: .milliseconds(20))
        await model.select(report.entries[1])
        await slowSelect

        #expect(model.selectedPath == "b.txt")
        #expect(model.diffState == .loaded("b.txt", .text(hunks: [laterHunk])))
        #expect(
            !shown.contains(.loaded("a.txt", .text(hunks: [staleHunk]))),
            "the slow selection's diff never rendered")
    }

    @Test func selectingAFileLoadsItsWorktreeDiff() async throws {
        await model.loadStatus()
        let hunk = DiffHunk(
            oldStart: 1, oldLines: 1, newStart: 1, newLines: 1,
            lines: [DiffLine(kind: .added, text: "x")])
        repo.lock.withLock {
            repo.worktreeDiffs["a.txt"] = .text(hunks: [hunk])
            repo.blobDiffs[FakeRepository.diffKey(old: oid(0x02), new: nil)] = .text(hunks: [])
        }

        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded")
            return
        }
        try #require(report.entries.count == 3)
        await model.select(report.entries[0])
        guard case let .loaded(path, diff) = model.diffState else {
            Issue.record("expected a loaded diff, got \(model.diffState)")
            return
        }
        #expect(path == "a.txt")
        #expect(diff == .text(hunks: [hunk]))

        // A deleted file diffs through the blob pair, old side only.
        await model.select(report.entries[2])
        guard case let .loaded(deletedPath, deletedDiff) = model.diffState else {
            Issue.record("expected a loaded diff for the deleted file")
            return
        }
        #expect(deletedPath == "gone.txt")
        #expect(deletedDiff == .text(hunks: []))

        // A vanished selection resets the pane on refresh.
        repo.lock.withLock {
            repo.statusReport = StatusReport(entries: [], scanned: 0, truncated: false)
        }
        await model.loadStatus()
        #expect(model.selectedPath == nil)
        #expect(model.diffState == .idle)
    }

    @Test func aRescanReloadsTheSelectedDiffWithoutShowingLoading() async throws {
        repo.lock.withLock { repo.worktreeDiffs["a.txt"] = .text(hunks: [Self.firstHunk]) }
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        await model.select(try #require(report.entries.first))
        var shown: [WorktreeDiffState] = []
        model.onDiffChanged = { shown.append(self.model.diffState) }

        let newDiff = BlobDiff.text(hunks: [Self.secondHunk])
        repo.lock.withLock { repo.worktreeDiffs["a.txt"] = newDiff }
        await model.loadStatus()
        #expect(model.diffState == .loaded("a.txt", newDiff))
        #expect(shown == [.loaded("a.txt", newDiff)], "never back through loading")

        await model.loadStatus()
        #expect(shown.count == 1, "an unchanged diff does not re-render")
    }

    @Test func aDiffThatCannotBeReadShowsTheErrorForThatPath() async throws {
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        var diffCallbacks = 0
        model.onDiffChanged = { diffCallbacks += 1 }
        await model.select(try #require(report.entries.dropFirst().first))
        guard case let .failed(path, message) = model.diffState else {
            Issue.record("expected a failed diff, got \(model.diffState)")
            return
        }
        #expect(path == "b.txt")
        #expect(message.contains("read working tree file"))
        #expect(diffCallbacks == 2, "one for loading, one for failed")
    }

    @Test func stashesLoad() async {
        repo.lock.withLock {
            repo.stashes = [StashEntry(index: 0, oid: oid(0x0C), message: "WIP on main: x")]
        }
        var stashCallbacks = 0
        model.onStashChanged = { stashCallbacks += 1 }
        await model.loadStashes()
        #expect(model.stashes.count == 1)
        #expect(model.stashState == .loaded)
        #expect(stashCallbacks == 1)
    }

    @Test func aFailedStashListSurfacesThroughOnErrorAndKeepsTheLastList() async {
        repo.lock.withLock {
            repo.stashes = [StashEntry(index: 0, oid: oid(0x0C), message: "WIP on main: x")]
        }
        await model.loadStashes()
        var reported: [String] = []
        model.onError = { reported.append($0) }
        var states: [StashListState] = []
        model.onStashChanged = { states.append(self.model.stashState) }
        repo.lock.withLock { repo.failNextStash = true }
        await model.loadStashes()

        #expect(reported.count == 1)
        #expect(reported.first?.contains("list stashes") == true)
        #expect(model.stashes.count == 1, "the last list read stays for the view to set aside")
        guard case let .failed(message) = model.stashState else {
            Issue.record("expected failed, got \(model.stashState)")
            return
        }
        #expect(message.contains("list stashes"))
        #expect(states == [model.stashState], "the failure is announced for inline display")

        await model.loadStashes()
        #expect(model.stashState == .loaded, "a successful read clears the failure")
        #expect(states.last == .loaded)
    }

    @Test func aStashFailureThatPersistsAcrossRefreshesAlertsOnce() async {
        var reported: [String] = []
        model.onError = { reported.append($0) }
        repo.lock.withLock { repo.failNextStash = true }
        await model.refresh()
        repo.lock.withLock { repo.failNextStash = true }
        await model.refresh()

        #expect(repo.lock.withLock { repo.stashCalls } == 2)
        #expect(reported.count == 1, "one alert per distinct message, not one per refresh")
        guard case .failed = model.stashState else {
            Issue.record("expected failed, got \(model.stashState)")
            return
        }
    }

    @Test func aBurstOfStashRequestsCostsOneReload() async {
        repo.lock.withLock { repo.stashDelay = 1 }
        async let inFlight: Void = model.loadStashes()
        try? await Task.sleep(for: .milliseconds(100))
        for _ in 0..<20 {
            await model.loadStashes()
        }
        await inFlight
        #expect(
            repo.lock.withLock { repo.stashCalls } == 2,
            "the read in flight plus one re-run, however many requests arrived")
    }

    @Test func aCancelledStashLoadNeverLands() async {
        repo.lock.withLock {
            repo.stashes = [StashEntry(index: 0, oid: oid(0x0C), message: "WIP on main: x")]
            repo.stashDelay = 1
        }
        var stashCallbacks = 0
        model.onStashChanged = { stashCallbacks += 1 }
        async let inFlight: Void = model.loadStashes()
        try? await Task.sleep(for: .milliseconds(100))
        model.cancelScans()
        await inFlight
        #expect(model.stashes.isEmpty, "the cancelled read's result never lands")
        #expect(stashCallbacks == 0)
        #expect(repo.lock.withLock { repo.stashCalls } == 1, "a cancel drops the queued re-run")
    }

    @Test func aRefreshBurstCostsTwoOfEach() async {
        let gate = CallGate()
        repo.statusGate = gate
        repo.lock.withLock { repo.stashDelay = 1 }
        async let inFlight: Void = model.refresh()
        await gate.waitUntilArrived()
        try? await Task.sleep(for: .milliseconds(100))
        for _ in 0..<20 {
            await model.refresh()
        }
        gate.open()
        await inFlight
        #expect(
            repo.lock.withLock { repo.statusCalls } == 2,
            "the scan in flight plus one re-run, however many refreshes arrived")
        #expect(
            repo.lock.withLock { repo.stashCalls } == 2,
            "the stash read in flight plus one re-run, however many refreshes arrived")
    }

    @Test func refreshLoadsTheChangeListAndTheStashes() async {
        let stashes = [StashEntry(index: 0, oid: oid(0x0C), message: "WIP on main: x")]
        repo.lock.withLock { repo.stashes = stashes }
        await model.refresh()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        #expect(report == repo.lock.withLock { repo.statusReport })
        #expect(model.stashes == stashes)
    }

    @Test func aReselectedFilesOlderDiffNeverLands() async throws {
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        let aTxt = try #require(report.entries.first)
        repo.lock.withLock {
            repo.worktreeDiffs["a.txt"] = .text(hunks: [Self.firstHunk])
            repo.worktreeDiffDelays["a.txt"] = 1
        }
        var shown: [WorktreeDiffState] = []
        model.onDiffChanged = { shown.append(self.model.diffState) }
        async let slowSelect: Void = model.select(aTxt)
        try? await Task.sleep(for: .milliseconds(100))
        // Same path, newer content: only the generation tells the two reads apart.
        repo.lock.withLock {
            repo.worktreeDiffs["a.txt"] = .text(hunks: [Self.secondHunk])
            repo.worktreeDiffDelays["a.txt"] = nil
        }
        await model.select(aTxt)
        await slowSelect

        #expect(model.diffState == .loaded("a.txt", .text(hunks: [Self.secondHunk])))
        #expect(
            !shown.contains(.loaded("a.txt", .text(hunks: [Self.firstHunk]))),
            "the older read never rendered")
    }

    @Test func aDiffForAFileThatLeftTheListNeverLands() async throws {
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        repo.lock.withLock {
            repo.worktreeDiffs["a.txt"] = .text(hunks: [Self.firstHunk])
            repo.worktreeDiffDelays["a.txt"] = 1
        }
        let aTxt = try #require(report.entries.first)
        async let slowSelect: Void = model.select(aTxt)
        try? await Task.sleep(for: .milliseconds(100))
        repo.lock.withLock { repo.statusReport = reportWith(aTxt: false) }
        await model.loadStatus()
        await slowSelect

        #expect(model.selectedPath == nil)
        #expect(model.diffState == .idle, "the vanished file's diff never lands")
    }
}
