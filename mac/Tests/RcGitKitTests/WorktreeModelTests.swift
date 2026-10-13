import Foundation
import Testing

@testable import RcGitKit

/// Browsing the working directory as a pseudo-ref.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct WorktreeModelTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let readme = oid(0x40)

    init() {
        repo.listing = RefListing(refs: [
            branch("main", at: mainCommit)
        ])
        repo.headValue = Head(refName: "refs/heads/main", target: mainCommit)
        repo.treeOfMap = [mainCommit: mainTree]
        repo.trees[mainTree] = [TreeEntry(name: "README.md", oid: readme, kind: .blob)]
        repo.worktreeListings[""] = WorktreeListing(
            entries: [
                WorktreeEntry(
                    name: "README.md", kind: .blob, status: .modified, oid: readme, size: 20),
                WorktreeEntry(name: "new.txt", kind: .blob, status: .untracked, oid: nil, size: 4),
                WorktreeEntry(name: "src", kind: .tree, status: .unchanged, oid: nil, size: 0),
            ],
            missing: [TreeEntry(name: "gone.txt", oid: oid(0x50), kind: .blob)]
        )
        repo.worktreeFiles["new.txt"] = textBlob("new\n")
    }

    @Test func aCheckoutOpensOnTheWorkingTree() async {
        repo.workDir = "/checkout"
        await model.attach(repo)
        #expect(model.browsingWorkingTree)
        #expect(model.selectedRef == nil)
        #expect(model.rootTree == mainTree)
    }

    @Test func anUnbornHeadOpensOnABranchInstead() async {
        repo.workDir = "/checkout"
        repo.headValue = Head(refName: "refs/heads/main", target: nil)
        await model.attach(repo)
        #expect(!model.browsingWorkingTree)
        #expect(model.selectedRef?.shortName == "main")
    }

    @Test func treeRowsHideClutterUntilAsked() {
        let store = BrowseRow(entry: TreeEntry(name: ".DS_Store", oid: oid(1), kind: .blob))
        let env = BrowseRow(
            entry: TreeEntry(name: ".env", oid: oid(2), kind: .blob), status: .ignored)
        #expect(!model.isVisible(store))
        #expect(model.isVisible(env), "a gitignored dotfile stays visible")
        model.setShowsHiddenFiles(true)
        #expect(model.isVisible(store))
    }

    @Test func pathSearchInTheWorkingTreeWalksDiskOncePerSession() async throws {
        repo.workDir = "/checkout"
        repo.worktreeFileList = WorktreeFileList(
            files: [
                WorktreeFile(path: ".DS_Store", kind: .blob),
                WorktreeFile(path: ".env", kind: .blob),
                WorktreeFile(path: "README.md", kind: .blob),
            ], scanned: 3, truncated: false)
        await model.attach(repo)

        let visible = try await model.searchPaths("")
        #expect(visible.map(\.path) == [".env", "README.md"], "gitignored .env is found")
        model.setShowsHiddenFiles(true)
        let all = try await model.searchPaths("")
        #expect(all.map(\.path) == [".DS_Store", ".env", "README.md"])
        #expect(repo.lock.withLock { repo.worktreeFilesCalls } == 1, "keystrokes reuse the walk")

        model.forgetWorktreePaths()
        _ = try await model.searchPaths("env")
        #expect(repo.lock.withLock { repo.worktreeFilesCalls } == 2)
    }

    @Test func pathSearchOnABranchSearchesItsTree() async throws {
        repo.workDir = "/checkout"
        await model.attach(repo)
        await model.select(model.refs.refs[0])
        let matches = try await model.searchPaths("")
        #expect(matches.map(\.path) == ["README.md"])
        #expect(repo.lock.withLock { repo.worktreeFilesCalls } == 0)
    }

    @Test func aSearchPickOpensFromDiskInTheWorkingTree() async {
        repo.workDir = "/checkout"
        await model.attach(repo)
        let pick = PathIndexEntry(path: "new.txt", oid: .unhashed, kind: .blob, hidden: false)
        await model.openSearchResult(pick)
        guard case let .text(entry, document) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.name == "new.txt")
        #expect(document.text == "new\n")
        #expect(model.openedFilePath == "new.txt")
    }

    @Test func selectingTheWorkingTreeRootsAtHead() async {
        await model.attach(repo)
        await model.selectWorkingTree()
        #expect(model.browsingWorkingTree)
        #expect(model.selectedRef == nil)
        #expect(model.rootTree == mainTree)

        await model.select(model.refs.refs[0])
        #expect(!model.browsingWorkingTree, "picking a ref leaves the working tree")
    }

    @Test func rowsMergeDiskAndMissingInDisplayOrder() async throws {
        await model.attach(repo)
        await model.selectWorkingTree()
        let rows = try await model.worktreeRows(at: "")
        #expect(
            rows.map(\.entry.name) == ["src", "gone.txt", "new.txt", "README.md"],
            "directories first, then names, deleted files in their place")
        #expect(rows[1].missing)
        #expect(rows[2].status == .untracked)
        #expect(rows[3].status == .modified)
        #expect(rows[3].entry.oid == readme, "a tracked file keeps its HEAD blob id")
    }

    @Test func worktreeFilesOpenFromDisk() async {
        await model.attach(repo)
        await model.selectWorkingTree()
        await model.openWorktreeFile(name: "new.txt", path: "new.txt")
        guard case let .text(entry, document) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.name == "new.txt")
        #expect(document.text == "new\n")
        #expect(model.openedFilePath == "new.txt")

        await model.openWorktreeFile(name: "gone.txt", path: "gone.txt")
        guard case let .failed(_, message) = model.fileState else {
            Issue.record("expected failure, got \(model.fileState)")
            return
        }
        #expect(message.contains("gone.txt"))
    }

    @Test func loadAnywayRereadsATooLargeDiskFileFromDisk() async {
        repo.worktreeFiles["big.txt"] = textBlob("big file\n")
        await model.attach(repo)
        await model.selectWorkingTree()
        await model.openWorktreeFile(name: "big.txt", path: "big.txt", limit: 2)
        guard case .tooLarge = model.fileState else {
            Issue.record("expected too large, got \(model.fileState)")
            return
        }

        await model.loadAnyway(limit: 1 << 20)
        guard case let .text(entry, document) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.name == "big.txt")
        #expect(document.text == "big file\n")
        #expect(model.openedFilePath == "big.txt")
        #expect(repo.lock.withLock { repo.readBlobCalls } == 0)
    }

    @Test func loadAnywayRereadsAMissingRowFromItsBlob() async {
        let gone = TreeEntry(name: "gone.txt", oid: oid(0x50), kind: .blob)
        repo.blobs[gone.oid] = textBlob("deleted on disk\n")
        await model.attach(repo)
        await model.selectWorkingTree()
        await model.openFile(gone, path: "gone.txt", limit: 2)
        #expect(model.fileState == .tooLarge(gone, size: 16, limit: 2))

        await model.loadAnyway(limit: 1 << 20)
        guard case let .text(entry, document) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry == gone, "a file gone from disk reads its HEAD blob, not the disk")
        #expect(document.text == "deleted on disk\n")
        #expect(model.openedFilePath == "gone.txt")
        #expect(repo.lock.withLock { repo.readBlobCalls } == 2)
    }

    @Test func refreshFollowsHeadWhileBrowsingTheWorkingTree() async {
        await model.attach(repo)
        await model.selectWorkingTree()
        var rootEvents = 0
        model.onRootChanged = { rootEvents += 1 }

        await model.refreshRefs()
        #expect(rootEvents == 0, "an unmoved HEAD does not reload")

        let movedCommit = oid(0x66)
        let movedTree = oid(0x67)
        repo.lock.withLock {
            repo.headValue = Head(refName: "refs/heads/main", target: movedCommit)
            repo.treeOfMap[movedCommit] = movedTree
            repo.trees[movedTree] = []
            repo.listing = RefListing(refs: [
                branch("main", at: movedCommit)
            ])
        }
        await model.refreshRefs()
        #expect(model.browsingWorkingTree, "the pseudo-ref stays selected")
        #expect(model.rootTree == movedTree)
        #expect(rootEvents == 1)
    }

    @Test func refreshToAnUnbornHeadLeavesTheWorkingTree() async {
        repo.workDir = "/checkout"
        await model.attach(repo)
        await model.selectWorkingTree()
        var refsEvents = 0
        model.onRefsChanged = { refsEvents += 1 }

        repo.lock.withLock {
            repo.headValue = Head(refName: "refs/heads/orphan", target: nil)
        }
        await model.refreshRefs()
        #expect(!model.browsingWorkingTree, "an unborn HEAD has no working tree to compare")
        #expect(model.selectedRef?.shortName == "main", "falls back the way attach chooses")
        #expect(model.rootTree == mainTree)
        #expect(refsEvents >= 1)
    }

    @Test func refreshToAnUnbornHeadWithNoRefsClearsTheRoot() async {
        repo.workDir = "/checkout"
        await model.attach(repo)
        await model.selectWorkingTree()
        var refsEvents = 0
        var rootEvents = 0
        model.onRefsChanged = { refsEvents += 1 }
        model.onRootChanged = { rootEvents += 1 }

        repo.lock.withLock {
            repo.headValue = Head(refName: "refs/heads/orphan", target: nil)
            repo.listing = RefListing(refs: [])
        }
        await model.refreshRefs()
        #expect(!model.browsingWorkingTree)
        #expect(model.selectedRef == nil)
        #expect(model.rootTree == nil)
        #expect(refsEvents == 1)
        #expect(rootEvents == 1)
    }
}
