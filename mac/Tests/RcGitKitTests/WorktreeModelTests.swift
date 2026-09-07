import Foundation
import Testing

@testable import RcGitKit

/// Browsing the working directory as a pseudo-ref.
@MainActor
@Suite struct WorktreeModelTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let readme = oid(0x40)

    init() {
        repo.listing = RefListing(refs: [
            RefEntry(
                fullName: "refs/heads/main", shortName: "main", kind: .localBranch,
                target: mainCommit)
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
                RefEntry(
                    fullName: "refs/heads/main", shortName: "main", kind: .localBranch,
                    target: movedCommit)
            ])
        }
        await model.refreshRefs()
        #expect(model.browsingWorkingTree, "the pseudo-ref stays selected")
        #expect(model.rootTree == movedTree)
        #expect(rootEvents == 1)
    }
}
