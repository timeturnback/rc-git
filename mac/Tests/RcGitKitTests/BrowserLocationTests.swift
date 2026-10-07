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
            RefEntry(
                fullName: feature, shortName: "feature", kind: .localBranch,
                target: featureCommit),
            RefEntry(fullName: main, shortName: "main", kind: .localBranch, target: mainCommit),
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
        repo.lock.withLock { repo.delays[mainCommit] = 1 }
        async let slowRestore: Void = model.restore(onMain)
        try? await Task.sleep(for: .milliseconds(20))
        await model.select(featureRef)
        await slowRestore
        #expect(model.selectedRef?.fullName == feature)
        #expect(model.openedFilePath == nil, "README.md also exists on feature; it stays shut")
    }

    @Test func aRestoreAskedForDuringAnotherIsTheOneThatStands() async {
        await model.attach(repo)
        let plain = model.location
        repo.lock.withLock { repo.delays[readme] = 1 }
        async let slowRestore: Void = model.restore(
            BrowserLocation(tree: .ref(main), filePath: "README.md", detailCommit: olderCommit))
        try? await Task.sleep(for: .milliseconds(20))
        await model.restore(plain)
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
        repo.lock.withLock { repo.delays[mainCommit] = 1 }
        var reported: [BrowserLocation] = []
        model.onLocationChanged = { reported.append(self.model.location) }
        async let slowRestore: Void = model.restore(atSource)
        try? await Task.sleep(for: .milliseconds(20))
        await model.restore(atReadme)
        await slowRestore
        #expect(model.location == atReadme, "opened against main's tree, once it had loaded")
        #expect(reported == [atReadme], "one report, where the pair ended")
    }
}
