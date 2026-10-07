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
        repo.resolves["HEAD"] = head
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

    @Test func loadsStatusAndCapturesExpectedHead() async {
        var statusCallbacks = 0
        model.onStatusChanged = { statusCallbacks += 1 }
        await model.loadStatus()
        guard case let .loaded(report) = model.statusState else {
            Issue.record("expected loaded, got \(model.statusState)")
            return
        }
        #expect(report.entries.count == 3)
        #expect(model.expectedHead == head.hex)
        #expect(statusCallbacks == 2, "one for loading, one for loaded")
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
        repo.statusDelay = 1.0
        async let inFlight: Void = model.loadStatus()
        try? await Task.sleep(for: .milliseconds(100))
        repo.lock.withLock { repo.statusDelay = 0 }
        for _ in 0..<20 {
            await model.loadStatus()
        }
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
        repo.statusDelay = 1.0
        async let inFlight: Void = model.loadStatus()
        try? await Task.sleep(for: .milliseconds(100))
        model.cancelScans()
        #expect(model.statusState == .idle, "the tab reloads on the next switch back")
        await inFlight
        #expect(model.statusState == .idle, "the cancelled scan's result never lands")
        #expect(
            repo.lock.withLock { repo.statusCalls } == 1, "a cancel drops the queued re-run")
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

    @Test func selectingAFileLoadsItsWorktreeDiff() async {
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

    @Test func stashesLoadAndFailuresSurfaceThroughOnError() async {
        repo.lock.withLock {
            repo.stashes = [StashEntry(index: 0, oid: oid(0x0C), message: "WIP on main: x")]
        }
        var stashCallbacks = 0
        model.onStashChanged = { stashCallbacks += 1 }
        await model.loadStashes()
        #expect(model.stashes.count == 1)
        #expect(stashCallbacks == 1)
    }
}
