import Foundation
import Testing

@testable import RcGitKit

/// What `BrowserModel` reports as its location, and how it returns to one.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct BrowserLocationTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let featureCommit = oid(0x21)
    let featureTree = oid(0x22)
    let olderCommit = oid(0x31)
    let srcTree = oid(0x30)
    let readme = oid(0x40)
    let source = oid(0x41)

    let main = "refs/heads/main"
    let feature = "refs/heads/feature"

    init() {
        repo.listing = RefListing(refs: [
            branch("feature", at: featureCommit),
            branch("main", at: mainCommit),
        ])
        repo.headValue = Head(refName: main, target: mainCommit)
        repo.treeOfMap = [
            mainCommit: mainTree, featureCommit: featureTree, olderCommit: mainTree,
        ]
        repo.trees[mainTree] = [
            TreeEntry(name: "src", oid: srcTree, kind: .tree),
            TreeEntry(name: "README.md", oid: readme, kind: .blob),
        ]
        repo.trees[featureTree] = [TreeEntry(name: "README.md", oid: readme, kind: .blob)]
        repo.trees[srcTree] = [TreeEntry(name: "main.rs", oid: source, kind: .blob)]
        repo.blobs[readme] = textBlob("# hi\n")
        repo.blobs[source] = textBlob("fn main() {}\n")
        repo.commits[mainCommit] = [
            commitSummary(mainCommit, parents: [olderCommit], title: "main"),
            commitSummary(olderCommit, title: "older"),
        ]
        repo.commits[featureCommit] = [commitSummary(featureCommit, title: "feature")]
        repo.treeDiffs[mainCommit] = []
        repo.treeDiffs[olderCommit] = []
        repo.pathHistories[FakeRepository.pathKey(tip: mainCommit, path: "README.md")] = [
            commitSummary(olderCommit, title: "older")
        ]
    }

    private var featureRef: RefEntry { model.refs.refs[0] }

    @Test func locationNamesTheTreeFileScopeAndCommit() async {
        #expect(model.location == BrowserLocation(), "nothing is open")
        await model.attach(repo)
        #expect(model.location == BrowserLocation(tree: .ref(main)))
        await model.openPath("src/main.rs")
        await model.showPathHistory("README.md")
        await model.loadCommitChanges(commitSummary(olderCommit))
        #expect(
            model.location
                == BrowserLocation(
                    tree: .ref(main), filePath: "src/main.rs", historyScope: .path("README.md"),
                    detailCommit: olderCommit))
    }

    @Test func aCheckoutOpensOnTheWorkingTreeAndReturnsToIt() async {
        repo.workDir = "/work"
        repo.worktreeFiles["README.md"] = textBlob("# on disk\n")
        repo.worktreeListings[""] = WorktreeListing(entries: [
            WorktreeEntry(name: "README.md", kind: .blob, status: .modified, oid: readme, size: 10)
        ])
        await model.attach(repo)
        #expect(model.location == BrowserLocation(tree: .workingTree))
        await model.openPath("README.md")
        let onDisk = model.location
        await model.select(featureRef)
        await model.restore(onDisk)
        #expect(model.browsingWorkingTree)
        #expect(model.location == onDisk)
        guard case let .text(_, document) = model.fileState else {
            Issue.record("expected the file from disk, got \(model.fileState)")
            return
        }
        #expect(document.text == "# on disk\n")
    }

    @Test func everyMoveReportsOnce() async {
        await model.attach(repo)
        var reports = 0
        model.onLocationChanged = { reports += 1 }
        await model.openPath("README.md")
        #expect(reports == 1)
        await model.openPath("README.md")
        #expect(reports == 1, "the same file again is not a move")
        await model.showPathHistory("README.md")
        #expect(reports == 2)
        await model.loadCommitChanges(commitSummary(olderCommit))
        #expect(reports == 3)
        model.closeCommitDetail()
        #expect(reports == 4)
        model.clearHistoryScope()
        #expect(reports == 5)
        await model.refreshRefs()
        #expect(reports == 5, "a refresh that moved nothing is not a move")
    }

    @Test func aRefSwitchReportsWhereItEndedAndNotTheStepBetween() async {
        await model.attach(repo)
        await model.openPath("README.md")
        await model.loadCommitChanges(commitSummary(olderCommit))
        var reported: [BrowserLocation] = []
        model.onLocationChanged = { reported.append(self.model.location) }
        await model.select(featureRef)
        #expect(
            reported == [BrowserLocation(tree: .ref(feature))],
            "never the new ref with the old file still open")
    }

    @Test func restoreReopensAnEarlierFileOfTheSameTree() async {
        await model.attach(repo)
        await model.openPath("README.md")
        let atReadme = model.location
        await model.openPath("src/main.rs")
        let treeReads = repo.treeEntryCalls
        await model.restore(atReadme)
        #expect(model.location == atReadme)
        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.oid == readme)
        #expect(repo.treeEntryCalls == treeReads, "the tree did not change, so none is read")
    }

    @Test func restoreClosesAFileTheLocationDidNotHave() async {
        await model.attach(repo)
        let noFile = model.location
        await model.openPath("README.md")
        var fileEvents = 0
        model.onFileChanged = { fileEvents += 1 }
        await model.restore(noFile)
        #expect(model.openedFilePath == nil)
        #expect(model.fileState == .empty)
        #expect(fileEvents == 1)
    }

    @Test func restoreSwitchesTheTreeBeforeReopeningItsFile() async {
        await model.attach(repo)
        await model.openPath("src/main.rs")
        let onMain = model.location
        await model.select(featureRef)
        await model.restore(onMain)
        #expect(model.selectedRef?.fullName == main)
        #expect(model.rootTree == mainTree)
        #expect(model.location == onMain)
        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.oid == source)
    }

    @Test func restoreBringsBackTheScopeAndTheCommitDetail() async {
        await model.attach(repo)
        let plain = model.location
        await model.showPathHistory("README.md")
        await model.loadCommitChanges(commitSummary(olderCommit))
        let scoped = model.location

        await model.restore(plain)
        #expect(model.historyScope == .all)
        #expect(model.diffCommit == nil)
        #expect(model.commitDetail == .empty)

        await model.restore(scoped)
        #expect(model.historyScope == .path("README.md"))
        #expect(model.diffCommit?.oid == olderCommit)
        #expect(model.history.map(\.oid) == [olderCommit], "the cached page shows at once")
    }

    @Test func restoreOfABrowsedCommitKeepsTheHistoryItWasPickedFrom() async {
        await model.attach(repo)
        await model.browseCommit(commitSummary(olderCommit))
        let atCommit = model.location
        #expect(atCommit.tree == .commit(olderCommit, historyTip: mainCommit))
        await model.select(featureRef)
        await model.restore(atCommit)
        #expect(model.browsedCommit?.oid == olderCommit)
        #expect(model.selectedRef == nil)
        #expect(model.location == atCommit)
        await model.loadMoreHistory()
        #expect(model.history.first?.oid == mainCommit, "main's history, not feature's")
    }

    @Test func restoreOfADeletedRefReportsAndChangesNothing() async {
        await model.attach(repo)
        let before = model.location
        var reported: String?
        model.onError = { reported = $0 }
        await model.restore(
            BrowserLocation(tree: .ref("refs/heads/gone"), filePath: "README.md"))
        #expect(reported == "cannot return to refs/heads/gone: the ref no longer exists")
        #expect(model.location == before)
    }

    @Test func restoreOfAPrunedCommitReportsAndChangesNothing() async {
        await model.attach(repo)
        let before = model.location
        var reported: String?
        model.onError = { reported = $0 }
        await model.restore(BrowserLocation(tree: .commit(oid(0x99), historyTip: mainCommit)))
        #expect(reported?.contains("read commit") == true)
        #expect(model.location == before)
    }

    @Test func restoreOfAFileThatIsGoneLeavesThePaneEmpty() async {
        await model.attach(repo)
        await model.openPath("README.md")
        await model.restore(BrowserLocation(tree: .ref(feature), filePath: "src/main.rs"))
        #expect(model.selectedRef?.fullName == feature)
        #expect(model.openedFilePath == nil, "feature has no src/main.rs")
        #expect(model.fileState == .empty)
    }

    @Test func restoreOfAFileGoneFromTheSameTreeClosesTheOneOpen() async {
        await model.attach(repo)
        await model.openPath("README.md")
        await model.restore(BrowserLocation(tree: .ref(main), filePath: "gone.txt"))
        #expect(model.openedFilePath == nil, "not README.md, which the stop never named")
        #expect(model.fileState == .empty)
    }

    @Test func restoreOfAFileGoneFromDiskClosesTheOneOpen() async {
        repo.workDir = "/work"
        repo.worktreeFiles["README.md"] = textBlob("# on disk\n")
        await model.attach(repo)
        await model.openPath("README.md")
        await model.restore(BrowserLocation(tree: .workingTree, filePath: "gone.txt"))
        #expect(model.openedFilePath == nil)
        #expect(model.fileState == .empty, "not the failed read of a file nobody asked for")
    }

    @Test func restoreOfAnInexactWorkingTreeFileReadsNothing() async {
        repo.workDir = "/work"
        let lossy = "caf\u{FFFD}.txt"
        repo.worktreeFiles["README.md"] = textBlob("# on disk\n")
        repo.worktreeFiles[lossy] = textBlob("a file literally named with U+FFFD\n")
        repo.worktreeListings[""] = WorktreeListing(entries: [
            WorktreeEntry(name: "README.md", kind: .blob, status: .modified, oid: readme, size: 10),
            WorktreeEntry(name: lossy, kind: .blob, status: .untracked, oid: nil, size: 30),
        ])
        await model.attach(repo)
        await model.openWorktreeFile(name: lossy, path: lossy, pathIsExact: false)
        let inexact = model.location
        #expect(inexact.filePath == lossy)
        #expect(!inexact.filePathIsExact)
        await model.openPath("README.md")
        let reads = repo.worktreeFileCalls
        await model.restore(inexact)
        #expect(repo.worktreeFileCalls == reads, "the stop's path names no one file")
        #expect(model.openedFilePath == nil)
        #expect(model.fileState == .empty)
    }

    @Test func restoreReportsWhereItEndedOnce() async {
        await model.attach(repo)
        await model.openPath("README.md")
        await model.showPathHistory("README.md")
        await model.loadCommitChanges(commitSummary(olderCommit))
        let full = model.location
        await model.select(featureRef)
        var reported: [BrowserLocation] = []
        model.onLocationChanged = { reported.append(self.model.location) }
        await model.restore(full)
        #expect(reported == [full])
    }

    @Test func aRefPickedDuringARestoreEndsTheRestore() async {
        await model.attach(repo)
        await model.openPath("README.md")
        let onMain = model.location
        await model.select(featureRef)
        let gate = CallGate()
        repo.lock.withLock { repo.gates[mainCommit] = gate }
        async let slowRestore: Void = model.restore(onMain)
        await gate.waitUntilArrived()
        await model.select(featureRef)
        gate.open()
        await slowRestore
        #expect(model.selectedRef?.fullName == feature)
        #expect(model.openedFilePath == nil, "README.md also exists on feature; it stays shut")
    }

    @Test func aRestoreAskedForDuringAnotherIsTheOneThatStands() async {
        await model.attach(repo)
        let plain = model.location
        let gate = CallGate()
        repo.lock.withLock { repo.gates[readme] = gate }
        async let slowRestore: Void = model.restore(
            BrowserLocation(tree: .ref(main), filePath: "README.md", detailCommit: olderCommit))
        await gate.waitUntilArrived()
        await model.restore(plain)
        gate.open()
        await slowRestore
        #expect(model.location == plain)
    }

    @Test func twoQuickRestoresIntoAnotherTreeEndOnTheSecond() async {
        await model.attach(repo)
        await model.openPath("README.md")
        let atReadme = model.location
        await model.openPath("src/main.rs")
        let atSource = model.location
        await model.select(featureRef)
        let gate = CallGate()
        repo.lock.withLock { repo.gates[mainCommit] = gate }
        var reported: [BrowserLocation] = []
        model.onLocationChanged = { reported.append(self.model.location) }
        async let slowRestore: Void = model.restore(atSource)
        await gate.waitUntilArrived()
        await model.restore(atReadme)
        gate.open()
        await slowRestore
        #expect(model.location == atReadme, "opened against main's tree, once it had loaded")
        #expect(reported == [atReadme], "one report, where the pair ended")
    }

    @Test func aRestoreEndingDuringARefreshLeavesNoReportForLater() async {
        await model.attach(repo)
        await model.openPath("README.md")
        let gate = CallGate()
        repo.lock.withLock { repo.refsGate = gate }
        async let refresh: Void = model.refreshRefs()
        await gate.waitUntilArrived()
        let restoring = Task {
            await model.restore(BrowserLocation(tree: .ref(main), filePath: "gone.txt"))
            model.onLocationChanged = {
                Issue.record("a report after the restore returned is a new stop")
            }
        }
        // The task runs up to the restore's wait for the held refresh before the gate opens.
        await Task.yield()
        gate.open()
        await restoring.value
        await refresh
        #expect(model.openedFilePath == nil)
    }

    @Test func aRestoreDuringARefreshOfAMovedRefKeepsItsFile() async {
        await model.attach(repo)
        await model.openPath("src/main.rs")
        let movedCommit = oid(0x13)
        repo.lock.withLock {
            repo.listing = RefListing(refs: [
                branch("feature", at: featureCommit),
                branch("main", at: movedCommit),
            ])
            repo.treeOfMap[movedCommit] = mainTree
        }
        let gate = CallGate()
        repo.lock.withLock { repo.gates[movedCommit] = gate }
        async let refresh: Void = model.refreshRefs()
        await gate.waitUntilArrived()
        let restoring = Task {
            await model.restore(BrowserLocation(tree: .ref(main), filePath: "README.md"))
            model.onLocationChanged = {
                Issue.record("a report after the restore returned is a new stop")
            }
        }
        // The task runs up to the restore's wait for the held refresh before the gate opens.
        await Task.yield()
        gate.open()
        await restoring.value
        await refresh
        #expect(model.selectedRef?.target == movedCommit)
        #expect(model.openedFilePath == "README.md", "the moved ref's root load closes no file")
    }

    @Test func aRefreshAskedForDuringARestoreRunsAfterIt() async {
        await model.attach(repo)
        await model.openPath("README.md")
        let onMain = model.location
        await model.select(featureRef)
        let restoreGate = CallGate()
        repo.lock.withLock { repo.gates[mainCommit] = restoreGate }
        async let slowRestore: Void = model.restore(onMain)
        await restoreGate.waitUntilArrived()
        let refreshGate = CallGate()
        repo.lock.withLock {
            repo.listing.refs.append(branch("topic", at: featureCommit))
            repo.refsGate = refreshGate
        }
        await model.refreshRefs()
        #expect(model.refs.refs.count == 2, "the restore is still opening main's tree")
        restoreGate.open()
        await slowRestore
        #expect(model.location == onMain)
        await refreshGate.waitUntilArrived()
        refreshGate.open()
        // Joins the refresh the restore started on its way out, and returns once it lands.
        await model.refreshRefs()
        #expect(model.refs.refs.count == 3, "the refresh ran once the restore was done")
    }
}
