import Foundation
import Testing

@testable import RcGitKit

/// How `BrowserModel` shares, cancels and retries the walks behind path search.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct PathIndexModelTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let featureCommit = oid(0x21)
    let featureTree = oid(0x22)
    let srcTree = oid(0x30)
    let readme = oid(0x40)

    init() {
        repo.listing = RefListing(refs: [
            branch("feature", at: featureCommit),
            branch("main", at: mainCommit),
        ])
        repo.headValue = Head(refName: "refs/heads/main", target: mainCommit)
        repo.treeOfMap = [mainCommit: mainTree, featureCommit: featureTree]
        repo.trees[mainTree] = [
            TreeEntry(name: "src", oid: srcTree, kind: .tree),
            TreeEntry(name: "README.md", oid: readme, kind: .blob),
        ]
        repo.trees[featureTree] = [TreeEntry(name: "src", oid: srcTree, kind: .tree)]
        repo.trees[srcTree] = [TreeEntry(name: "main.rs", oid: readme, kind: .blob)]
        repo.worktreeFileList = WorktreeFileList(
            files: [WorktreeFile(path: "README.md", kind: .blob)], scanned: 1, truncated: false)
    }

    /// Suspends until the fake has seen `count` calls of the kind `calls` reads.
    func waitForCalls(_ count: Int, _ calls: (FakeRepository) -> Int) async throws {
        while repo.lock.withLock({ calls(repo) }) < count {
            try await Task.sleep(for: .milliseconds(5))
        }
    }

    @Test func concurrentSearchesShareOneTreeWalk() async throws {
        await model.attach(repo)
        repo.delays[mainTree] = 1
        async let first = model.searchPaths("")
        async let second = model.searchPaths("")
        let (a, b) = try await (first, second)
        #expect(a.entries.map(\.path) == ["src/main.rs", "README.md"])
        #expect(b.entries.map(\.path) == a.entries.map(\.path))
        #expect(repo.lock.withLock { repo.treeEntryCalls } == 2, "one read per directory")
    }

    @Test func aBuildForAnotherRootCancelsTheRunningBuild() async throws {
        await model.attach(repo)
        repo.delays[mainTree] = 1
        let slow = Task { try await model.pathIndex(for: mainTree) }
        try await waitForCalls(1) { $0.treeEntryCalls }

        let other = try await model.pathIndex(for: featureTree)
        #expect(other.entries.map(\.path) == ["src/main.rs"])
        await #expect(throws: CancellationError.self) { _ = try await slow.value }
    }

    @Test func selectingAnotherRefCancelsTheRunningBuild() async throws {
        await model.attach(repo)
        repo.delays[mainTree] = 1
        let slow = Task { try await model.pathIndex(for: mainTree) }
        try await waitForCalls(1) { $0.treeEntryCalls }

        await model.select(model.refs.refs[0])
        #expect(model.rootTree == featureTree)
        let result = try await model.searchPaths("")
        #expect(result.entries.map(\.path) == ["src/main.rs"])
        await #expect(throws: CancellationError.self) { _ = try await slow.value }
    }

    @Test func concurrentWorkingTreeSearchesShareOneWalk() async throws {
        repo.workDir = "/checkout"
        await model.attach(repo)
        repo.worktreeFilesDelay = 1
        async let first = model.searchPaths("")
        async let second = model.searchPaths("")
        let (a, b) = try await (first, second)
        #expect(a.entries.map(\.path) == ["README.md"])
        #expect(b.entries.map(\.path) == ["README.md"])
        #expect(repo.lock.withLock { repo.worktreeFilesCalls } == 1)
    }

    @Test func aFailedWorkingTreeWalkIsRetriedByTheNextSearch() async throws {
        repo.workDir = "/checkout"
        await model.attach(repo)
        repo.failNextWorktreeFiles = true
        await #expect(throws: RepositoryError.self) { _ = try await model.searchPaths("") }
        let result = try await model.searchPaths("")
        #expect(result.entries.map(\.path) == ["README.md"])
        #expect(repo.lock.withLock { repo.worktreeFilesCalls } == 2)
    }

    @Test func anOutdatedWorkingTreeWalkAnswersItsQueryButIsNotKept() async throws {
        repo.workDir = "/checkout"
        await model.attach(repo)
        repo.worktreeFilesDelay = 1
        let slow = Task { try await model.searchPaths("") }
        try await waitForCalls(1) { $0.worktreeFilesCalls }

        model.forgetWorktreePaths()
        let answered = try await slow.value
        #expect(answered.entries.map(\.path) == ["README.md"])
        repo.lock.withLock { repo.worktreeFilesDelay = 0 }
        _ = try await model.searchPaths("")
        #expect(repo.lock.withLock { repo.worktreeFilesCalls } == 2, "the outdated walk is dropped")
    }
}
