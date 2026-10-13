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
        #expect(!model.refsStale)
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
        #expect(await model.openPath("src/main.rs") == .opened)
        #expect(model.openedFilePath == "src/main.rs")
        guard case let .text(entry, _) = model.fileState else {
            Issue.record("expected text state")
            return
        }
        #expect(entry.name == "main.rs")
        #expect(await model.openPath("src/missing.rs") == .missing)
        #expect(await model.openPath("src") == .missing)
        #expect(await model.openPath("src/main.rs/deeper") == .missing)
        #expect(model.openedFilePath == "src/main.rs")
    }

    @Test func openPathReportsAFailedTreeReadApartFromADeadLink() async {
        await model.attach(repo)
        #expect(await model.openPath("README.md") == .opened)
        let before = model.location
        var reports = 0
        model.onLocationChanged = { reports += 1 }
        repo.failNextTree = true
        let outcome = await model.openPath("src/main.rs")
        guard case let .failed(message) = outcome else {
            Issue.record("expected a failure, got \(outcome)")
            return
        }
        #expect(message.contains("read tree"), "the core message names the operation")
        #expect(model.location == before)
        #expect(reports == 0)
    }

    @Test func loadAnywayReadsTheWorkingTreeFile() async {
        let big = String(repeating: "x", count: Int(Repository.defaultBlobLimit) + 1)
        repo.worktreeFiles["big.txt"] = textBlob(big)
        await model.attach(repo)
        await model.selectWorkingTree()
        await model.openWorktreeFile(name: "big.txt", path: "big.txt")
        guard case let .tooLarge(_, size, limit) = model.fileState else {
            Issue.record("expected too large, got \(model.fileState)")
            return
        }
        #expect(limit == Repository.defaultBlobLimit)

        await model.loadAnyway(limit: size)
        guard case let .text(entry, document) = model.fileState else {
            Issue.record("expected text, got \(model.fileState)")
            return
        }
        #expect(entry.name == "big.txt")
        #expect(document.byteCount == big.utf8.count)
        #expect(repo.lock.withLock { repo.readBlobCalls } == 0, "the disk file is no blob")
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

    /// Stubs a one-range blame for `path` owned by `mainCommit`.
    private func stubBlame(_ path: String) {
        repo.blames[path] = .ranges([BlameRange(commit: mainCommit, startLine: 1, lineCount: 1)])
        repo.summaries[mainCommit] = commitSummary(mainCommit, title: "main")
    }

    /// A name that arrived with U+FFFD replacements, and its valid twin's spelling.
    private let inexact = "f\u{FFFD}o"

    @Test func inexactNameBlameIsRefusedInsteadOfWalkingTheTwin() async {
        await model.attach(repo)
        stubBlame(inexact)
        await model.openFile(
            TreeEntry(name: inexact, oid: readme, kind: .blob, hasExactName: false),
            path: inexact)
        await model.requestBlame()
        #expect(model.blame == .failed("cannot blame 'f\u{FFFD}o': the path is not valid UTF-8"))
        #expect(repo.blameLimits.isEmpty, "the twin's ranges were never read")
    }

    @Test func inexactNameInAFolderRefusesBlameForEveryFileBelow() async {
        await model.attach(repo)
        stubBlame("\(inexact)/main.rs")
        await model.openFile(
            TreeEntry(name: "main.rs", oid: readme, kind: .blob), path: "\(inexact)/main.rs",
            pathIsExact: false)
        #expect(!model.openedPathIsExact)
        await model.requestBlame()
        guard case let .failed(message) = model.blame else {
            Issue.record("expected a refusal, got \(model.blame)")
            return
        }
        #expect(message.hasSuffix("the path is not valid UTF-8"))
        #expect(repo.blameLimits.isEmpty)

        await model.openFile(
            TreeEntry(name: "README.md", oid: readme, kind: .blob), path: "README.md")
        #expect(model.openedPathIsExact, "an exact file opened later is blamed again")
    }

    @Test func inexactNameHistoryAndCopyAreRefused() async {
        await model.attach(repo)
        var errors: [String] = []
        model.onError = { errors.append($0) }
        await model.showPathHistory(inexact, pathIsExact: false)
        #expect(model.historyScope == .all, "the scope never moved to the twin's path")
        #expect(model.copyablePath(inexact, pathIsExact: false) == nil)
        #expect(model.copyablePath("README.md", pathIsExact: true) == "README.md")
        #expect(
            errors == [
                "cannot show the history of 'f\u{FFFD}o': the path is not valid UTF-8",
                "cannot copy the path of 'f\u{FFFD}o': the path is not valid UTF-8",
            ])
    }

    @Test func inexactNameNeverAnswersALinkByPath() async {
        repo.trees[mainTree] = [
            TreeEntry(name: inexact, oid: binary, kind: .blob, hasExactName: false),
            TreeEntry(name: "README.md", oid: readme, kind: .blob),
        ]
        await model.attach(repo)
        #expect(
            await model.openPath(inexact) == .missing,
            "the U+FFFD spelling names no stored file")
    }

    @Test func staleBlameIsDroppedAfterTheOpenFileChanges() async {
        await model.attach(repo)
        stubBlame("README.md")
        await model.openFile(
            TreeEntry(name: "README.md", oid: readme, kind: .blob), path: "README.md")
        var events = 0
        model.onBlameChanged = { events += 1 }
        let gate = CallGate()
        repo.blameGate = gate
        let readmeBlame = Task { await model.requestBlame() }
        await gate.waitUntilArrived()
        await model.openFile(
            TreeEntry(name: "main.rs", oid: readme, kind: .blob), path: "src/main.rs")
        gate.open()
        await readmeBlame.value

        #expect(events == 0, "the README walk finished after the user opened another file")
        #expect(model.blame == .empty)
    }

    @Test func aSupersededBlameLeavesTheNewerWalkInFlight() async {
        await model.attach(repo)
        stubBlame("README.md")
        stubBlame("src/main.rs")
        await model.openFile(
            TreeEntry(name: "README.md", oid: readme, kind: .blob), path: "README.md")
        var states: [BlameState] = []
        model.onBlameChanged = { states.append(self.model.blame) }
        let readmeGate = CallGate()
        repo.blameGate = readmeGate
        let readmeBlame = Task { await model.requestBlame() }
        await readmeGate.waitUntilArrived()

        await model.openFile(
            TreeEntry(name: "main.rs", oid: readme, kind: .blob), path: "src/main.rs")
        let mainGate = CallGate()
        repo.blameGate = mainGate
        let mainBlame = Task { await model.requestBlame() }
        await mainGate.waitUntilArrived()
        readmeGate.open()
        await readmeBlame.value
        #expect(states.isEmpty, "the README walk was superseded by the main.rs request")

        await model.requestBlame()
        mainGate.open()
        await mainBlame.value
        let spans = [
            BlameSpan(
                range: BlameRange(commit: mainCommit, startLine: 1, lineCount: 1),
                commit: commitSummary(mainCommit, title: "main"))
        ]
        #expect(states == [.loaded(path: "src/main.rs", spans: spans)])
        #expect(repo.blameLimits.count == 2, "the repeat joined the main.rs walk")
    }

    @Test func repeatedBlameRequestsShareOneWalk() async {
        await model.attach(repo)
        stubBlame("README.md")
        await model.openFile(
            TreeEntry(name: "README.md", oid: readme, kind: .blob), path: "README.md")
        var events = 0
        model.onBlameChanged = { events += 1 }
        let gate = CallGate()
        repo.blameGate = gate
        let first = Task { await model.requestBlame() }
        await gate.waitUntilArrived()
        await model.requestBlame()
        await model.requestBlame()
        gate.open()
        await first.value

        #expect(repo.blameLimits.count == 1)
        #expect(events == 1)
        guard case let .loaded(path, spans) = model.blame else {
            Issue.record("expected a loaded blame, got \(model.blame)")
            return
        }
        #expect(path == "README.md")
        #expect(spans.count == 1)
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

    @Test func selectingARefWhoseTreeCannotBeReadKeepsThePaneConsistent() async {
        await model.attach(repo)
        #expect(await model.openPath("README.md") == .opened)
        let before = model.location
        var reported: [String] = []
        model.onError = { reported.append($0) }
        let blobTag = RefEntry(
            fullName: "refs/tags/blob", shortName: "blob", kind: .tag, target: oid(0x70))
        await model.select(blobTag)
        #expect(reported.count == 1)
        #expect(reported.first?.contains("expected a tree") == true)
        #expect(model.selectedRef?.shortName == "main", "the click does not take")
        #expect(model.rootTree == mainTree)
        #expect(model.openedFilePath == "README.md")
        #expect(model.location == before)

        await model.browseCommit(commitSummary(oid(0x71), title: "tree missing"))
        #expect(reported.count == 2)
        #expect(model.browsedCommit == nil)
        #expect(model.selectedRef?.shortName == "main")
        #expect(model.location == before)
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
    }
}
