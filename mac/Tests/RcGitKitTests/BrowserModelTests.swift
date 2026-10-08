import Foundation
import Testing

@testable import RcGitKit

@MainActor
@Suite(.timeLimit(.minutes(1))) struct BrowserModelTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let featureCommit = oid(0x21)
    let featureTree = oid(0x22)
    let srcTree = oid(0x30)
    let readme = oid(0x40)
    let binary = oid(0x41)

    init() {
        repo.listing = RefListing(refs: [
            branch("feature", at: featureCommit),
            branch("main", at: mainCommit),
            RefEntry(fullName: "refs/tags/v1", shortName: "v1", kind: .tag, target: mainCommit),
        ])
        repo.headValue = Head(refName: "refs/heads/main", target: mainCommit)
        repo.treeOfMap = [mainCommit: mainTree, featureCommit: featureTree]
        repo.trees[mainTree] = [
            TreeEntry(name: "src", oid: srcTree, kind: .tree),
            TreeEntry(name: "README.md", oid: readme, kind: .blob),
            TreeEntry(name: "logo.png", oid: binary, kind: .blob),
        ]
        repo.trees[featureTree] = [TreeEntry(name: "src", oid: srcTree, kind: .tree)]
        repo.trees[srcTree] = [TreeEntry(name: "main.rs", oid: readme, kind: .blob)]
        repo.blobs[readme] = textBlob("# hi\n\nbody\n")
        repo.blobs[binary] = .binary(size: 42)
    }

    @Test func attachSelectsHeadBranchAndLoadsItsRoot() async {
        var refsEvents = 0
        var rootEvents = 0
        model.onRefsChanged = { refsEvents += 1 }
        model.onRootChanged = { rootEvents += 1 }
        await model.attach(repo)
        #expect(model.refs.refs.count == 3)
        #expect(model.head?.refName == "refs/heads/main")
        #expect(model.selectedRef?.shortName == "main")
        #expect(model.rootTree == mainTree)
        #expect(refsEvents == 2, "once for the listing, once for the selection")
        #expect(rootEvents == 1)
        #expect(model.lastError == nil)
    }

    @Test func initialRefFallsBackFromDetachedHead() {
        let listing = repo.listing
        let onBranch = model.initialRef(
            listing: listing, head: Head(refName: "refs/heads/feature", target: featureCommit))
        #expect(onBranch?.shortName == "feature")
        let detachedAtMain = model.initialRef(
            listing: listing, head: Head(refName: nil, target: mainCommit))
        #expect(detachedAtMain?.shortName == "main", "first ref pointing at the same commit")
        let unrelated = model.initialRef(
            listing: listing, head: Head(refName: nil, target: oid(0x99)))
        #expect(unrelated?.shortName == "feature", "first local branch")
        let empty = model.initialRef(
            listing: RefListing(refs: []), head: Head(refName: nil, target: nil))
        #expect(empty == nil)
    }

    @Test func childrenAreCachedByTreeIdAcrossRefs() async throws {
        await model.attach(repo)
        let first = try await model.children(of: srcTree)
        let second = try await model.children(of: srcTree)
        #expect(first == second)
        #expect(repo.treeEntryCalls == 1, "second call served from the cache")

        await model.select(model.refs.refs[0])
        #expect(model.rootTree == featureTree)
        _ = try await model.children(of: srcTree)
        #expect(repo.treeEntryCalls == 1, "same tree id under another ref is the same directory")
        #expect(model.trees.count == 1)
    }

    @Test func childrenErrorsSurfaceAsRepositoryError() async {
        await model.attach(repo)
        repo.failNextTree = true
        do {
            _ = try await model.children(of: srcTree)
            Issue.record("expected a throw")
        } catch let error as RepositoryError {
            #expect(error.message.contains("read tree"))
        } catch {
            Issue.record("unexpected error \(error)")
        }
        #expect(model.trees.cached(srcTree) == nil, "a failed load is not cached")
    }

    @Test func openPathWalksTheTreeAndReportsDeadLinks() async {
        await model.attach(repo)
        #expect(await model.openPath("src/main.rs"))
        #expect(model.openedFilePath == "src/main.rs")
        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text state")
            return
        }
        #expect(entry.name == "main.rs")
        #expect(!(await model.openPath("src/missing.rs")))
        #expect(!(await model.openPath("src")))
        #expect(!(await model.openPath("src/main.rs/deeper")))
        #expect(model.openedFilePath == "src/main.rs")
    }

    @Test func openFileProducesTextBinaryAndTooLargeStates() async {
        await model.attach(repo)
        let readmeEntry = TreeEntry(name: "README.md", oid: readme, kind: .blob)
        await model.openFile(readmeEntry)
        guard case let .text(entry, document) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry == readmeEntry)
        #expect(document.lineCount == 3)

        await model.openFile(TreeEntry(name: "logo.png", oid: binary, kind: .blob))
        #expect(
            model.fileState
                == .binary(TreeEntry(name: "logo.png", oid: binary, kind: .blob), size: 42))

        await model.openFile(readmeEntry, limit: 2)
        #expect(model.fileState == .tooLarge(readmeEntry, size: 11, limit: 2))

        let missing = TreeEntry(name: "gone", oid: oid(0x77), kind: .blob)
        await model.openFile(missing)
        guard case let .failed(failedEntry, message) = model.fileState else {
            Issue.record("expected failure, got \(model.fileState)")
            return
        }
        #expect(failedEntry == missing)
        #expect(message.contains("read blob"))

        await model.openFile(TreeEntry(name: "src", oid: srcTree, kind: .tree))
        #expect(model.fileState == .failed(missing, message), "directories are ignored")
    }

    @Test func staleFileLoadIsDropped() async {
        await model.attach(repo)
        let slow = oid(0x50)
        repo.blobs[slow] = textBlob("slow")
        let gate = CallGate()
        repo.gates[slow] = gate
        let slowEntry = TreeEntry(name: "slow.txt", oid: slow, kind: .blob)
        let fastEntry = TreeEntry(name: "README.md", oid: readme, kind: .blob)

        var states: [FileState] = []
        model.onFileChanged = { states.append(self.model.fileState) }
        async let slowLoad: Void = model.openFile(slowEntry)
        await gate.waitUntilArrived()
        await model.openFile(fastEntry)
        gate.open()
        await slowLoad

        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry == fastEntry, "the later request wins even though it finished first")
        #expect(states.contains(.loading(slowEntry)))
        #expect(
            !states.contains(where: {
                if case .text(slowEntry, _) = $0 { return true } else { return false }
            }))
    }

    @Test func staleRootLoadIsDropped() async {
        await model.attach(repo)
        let slowCommit = oid(0x60)
        let slowTree = oid(0x61)
        repo.treeOfMap[slowCommit] = slowTree
        repo.trees[slowTree] = []
        let slowRef = branch("slow", at: slowCommit)
        let gate = CallGate()
        repo.gates[slowCommit] = gate
        async let slowSelect: Void = model.select(slowRef)
        await gate.waitUntilArrived()
        await model.select(model.refs.refs[0])
        gate.open()
        await slowSelect
        #expect(model.selectedRef?.shortName == "feature")
        #expect(model.rootTree == featureTree)
    }

    @Test func attachFailureReportsAnError() async {
        let broken = FakeRepository()
        var reported: String?
        model.onError = { reported = $0 }
        await model.attach(broken)
        #expect(model.selectedRef == nil)
        #expect(reported == nil, "an empty repository is not an error")

        broken.failNextRefs = true
        await model.attach(broken)
        #expect(reported == "cannot list refs: transient lock")
        #expect(model.lastError == "cannot list refs: transient lock")
    }
}
