import Foundation
import Testing

@testable import RcGitKit

/// History paging, commit browsing, and the refs refresh driven by the file watcher.
@MainActor
@Suite struct BrowserHistoryTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let featureCommit = oid(0x21)
    let featureTree = oid(0x22)

    init() {
        repo.listing = RefListing(refs: [
            RefEntry(
                fullName: "refs/heads/feature", shortName: "feature", kind: .localBranch,
                target: featureCommit),
            RefEntry(
                fullName: "refs/heads/main", shortName: "main", kind: .localBranch,
                target: mainCommit),
        ])
        repo.headValue = Head(refName: "refs/heads/main", target: mainCommit)
        repo.treeOfMap = [mainCommit: mainTree, featureCommit: featureTree]
        repo.trees[mainTree] = []
        repo.trees[featureTree] = []
        repo.commits[mainCommit] = (0..<150).map { index in
            commitSummary(oid(UInt8(100 + index % 100)), title: "main \(index)")
        }
        repo.commits[featureCommit] = [commitSummary(featureCommit, title: "feature 0")]
    }

    @Test func historyLoadsInPagesAndReportsCompletion() async {
        await model.attach(repo)
        #expect(model.history.isEmpty)
        await model.loadMoreHistory()
        #expect(model.history.count == 100)
        #expect(!model.historyComplete)
        await model.loadMoreHistory()
        #expect(model.history.count == 150)
        #expect(model.historyComplete, "a short page means the walk is done")
        await model.loadMoreHistory()
        #expect(model.history.count == 150, "a complete history never re-fetches")
    }

    @Test func historyIsCachedPerTipAcrossReselection() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        #expect(model.history.count == 100)
        await model.select(model.refs.refs[0])
        #expect(model.history.isEmpty, "another tip starts fresh")
        await model.loadMoreHistory()
        #expect(model.history.count == 1)
        #expect(model.historyComplete)
        await model.select(model.refs.refs[1])
        #expect(model.history.count == 100, "cached pages come back without a walk")
    }

    @Test func historyErrorsSurface() async {
        await model.attach(repo)
        repo.lock.withLock { repo.commits[mainCommit] = nil }
        var reported: String?
        model.onError = { reported = $0 }
        await model.loadMoreHistory()
        #expect(reported?.contains("start commit walk") == true)
        #expect(!model.isLoadingHistory)
    }

    @Test func browsingACommitDetachesTheSelectionAndKeepsHistory() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        let commit = commitSummary(featureCommit, title: "feature 0")
        await model.browseCommit(commit)
        #expect(model.selectedRef == nil)
        #expect(model.browsedCommit == commit)
        #expect(model.rootTree == featureTree)
        #expect(model.history.count == 100, "the history pane keeps showing the ref's walk")
        await model.select(model.refs.refs[1])
        #expect(model.browsedCommit == nil)
        #expect(model.rootTree == mainTree)
    }

    @Test func refreshKeepsTheSelectionWhenItsTargetIsUnchanged() async {
        await model.attach(repo)
        var rootEvents = 0
        model.onRootChanged = { rootEvents += 1 }
        repo.lock.withLock {
            repo.listing.refs.append(
                RefEntry(
                    fullName: "refs/tags/v2", shortName: "v2", kind: .tag, target: mainCommit))
        }
        await model.refreshRefs()
        #expect(model.refs.refs.count == 3)
        #expect(model.selectedRef?.shortName == "main")
        #expect(rootEvents == 0, "an unchanged target must not reload the tree")
    }

    @Test func refreshReloadsWhenTheSelectedRefMoved() async {
        await model.attach(repo)
        let movedTree = oid(0x33)
        repo.lock.withLock {
            repo.listing.refs[1] = RefEntry(
                fullName: "refs/heads/main", shortName: "main", kind: .localBranch,
                target: featureCommit)
            repo.treeOfMap[featureCommit] = movedTree
            repo.trees[movedTree] = []
            repo.commits[featureCommit] = [commitSummary(featureCommit, title: "feature 0")]
        }
        var rootEvents = 0
        model.onRootChanged = { rootEvents += 1 }
        await model.refreshRefs()
        #expect(model.selectedRef?.target == featureCommit)
        #expect(model.rootTree == movedTree)
        #expect(rootEvents == 1)
        #expect(model.history.isEmpty, "history follows the new tip")
    }

    @Test func refreshFallsBackWhenTheSelectedRefDisappeared() async {
        await model.attach(repo)
        repo.lock.withLock {
            repo.listing.refs.removeAll { $0.shortName == "main" }
            repo.headValue = Head(refName: "refs/heads/feature", target: featureCommit)
        }
        await model.refreshRefs()
        #expect(model.selectedRef?.shortName == "feature")
        #expect(model.rootTree == featureTree)
    }

    @Test func refreshFailureKeepsTheLastGoodListingQuietly() async {
        await model.attach(repo)
        let failing = repo
        failing.lock.withLock { failing.failNextRefs = true }
        var reported: String?
        model.onError = { reported = $0 }
        await model.refreshRefs()
        #expect(reported == nil, "transient lock errors during a ref update must not alert")
        #expect(model.lastError?.contains("refs") == true)
        #expect(model.refs.refs.count == 2)
        #expect(model.selectedRef?.shortName == "main")
    }
}

/// Commit detail: changed files, one file's diff, and blame for the open file.
@MainActor
@Suite struct BrowserDiffTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let parentCommit = oid(0x10)
    let oldBlob = oid(0x41)
    let newBlob = oid(0x42)

    init() {
        repo.listing = RefListing(refs: [
            RefEntry(
                fullName: "refs/heads/main", shortName: "main", kind: .localBranch,
                target: mainCommit)
        ])
        repo.headValue = Head(refName: "refs/heads/main", target: mainCommit)
        repo.treeOfMap = [mainCommit: mainTree]
        repo.trees[mainTree] = [TreeEntry(name: "a.txt", oid: newBlob, kind: .blob)]
        repo.blobs[newBlob] = textBlob("one\n")
        let change = TreeChange(
            kind: .modified, path: "a.txt", sourcePath: nil, old: oldBlob, new: newBlob,
            entry: .blob)
        repo.treeDiffs[mainCommit] = [change]
        repo.blobDiffs[FakeRepository.diffKey(old: oldBlob, new: newBlob)] = .text(hunks: [
            DiffHunk(
                oldStart: 1, oldLines: 1, newStart: 1, newLines: 1,
                lines: [
                    DiffLine(kind: .removed, text: "zero"), DiffLine(kind: .added, text: "one"),
                ])
        ])
        repo.blames["a.txt"] = [BlameRange(commit: parentCommit, startLine: 1, lineCount: 1)]
        repo.summaries[parentCommit] = commitSummary(parentCommit, title: "parent")
    }

    @Test func selectingACommitLoadsItsChangesAndFirstDiff() async {
        await model.attach(repo)
        var events = 0
        model.onDiffChanged = { events += 1 }
        let commit = commitSummary(mainCommit, parents: [parentCommit])
        await model.loadCommitChanges(commit)
        #expect(model.diffCommit == commit)
        #expect(model.commitChanges.count == 1)
        guard case let .loaded(change, .text(hunks)) = model.fileDiff else {
            Issue.record("expected a loaded diff, got \(model.fileDiff)")
            return
        }
        #expect(change.path == "a.txt")
        #expect(hunks.count == 1)
        #expect(events >= 2, "cleared, then loaded")
    }

    @Test func selectingACommitLoadsItsDetailAndRefBadges() async {
        await model.attach(repo)
        let commit = commitSummary(mainCommit, parents: [parentCommit])
        let detail = CommitDetail(
            summary: commit, body: "The body.", committerName: "Test Committer",
            committerEmail: "committer@example.com", committerTime: 1_704_326_400,
            committerOffset: 0, tree: mainTree)
        repo.lock.withLock { repo.details[mainCommit] = detail }
        await model.loadCommitChanges(commit)
        guard case let .loaded(loaded, refs) = model.commitDetail else {
            Issue.record("expected a loaded detail, got \(model.commitDetail)")
            return
        }
        #expect(loaded == detail)
        #expect(refs.map(\.shortName) == ["main"], "the main branch points at this commit")
    }

    @Test func jumpingToAParentPushesTheBackStack() async {
        await model.attach(repo)
        repo.lock.withLock { repo.treeDiffs[parentCommit] = [] }
        await model.loadCommitChanges(commitSummary(mainCommit, parents: [parentCommit]))
        await model.jumpToCommit(parentCommit)
        #expect(model.diffCommit?.oid == parentCommit)
        #expect(model.detailBackStack.map(\.oid) == [mainCommit])
        await model.goBackInDetail()
        #expect(model.diffCommit?.oid == mainCommit)
        #expect(model.detailBackStack.isEmpty)
        await model.goBackInDetail()
        #expect(model.diffCommit?.oid == mainCommit, "an empty stack is a no-op")
    }

    @Test func switchingRefsClearsTheBackStack() async {
        await model.attach(repo)
        repo.lock.withLock { repo.treeDiffs[parentCommit] = [] }
        await model.loadCommitChanges(commitSummary(mainCommit, parents: [parentCommit]))
        await model.jumpToCommit(parentCommit)
        #expect(!model.detailBackStack.isEmpty)
        await model.select(model.refs.refs[0])
        #expect(model.detailBackStack.isEmpty)
        guard case .empty = model.commitDetail else {
            Issue.record("expected an empty detail after a ref switch")
            return
        }
    }

    @Test func commitWithNoChangesShowsEmptyDetail() async {
        await model.attach(repo)
        let empty = oid(0x77)
        repo.lock.withLock { repo.treeDiffs[empty] = [] }
        await model.loadCommitChanges(commitSummary(empty))
        #expect(model.commitChanges.isEmpty)
        guard case .empty = model.fileDiff else {
            Issue.record("expected empty detail")
            return
        }
    }

    @Test func diffErrorsSurface() async {
        await model.attach(repo)
        var reported: String?
        model.onError = { reported = $0 }
        await model.loadCommitChanges(commitSummary(oid(0x99)))
        #expect(reported?.contains("read tree") == true)

        let broken = TreeChange(
            kind: .modified, path: "b.txt", sourcePath: nil, old: oid(0x51), new: oid(0x52),
            entry: .blob)
        await model.loadFileDiff(broken)
        guard case let .failed(change, message) = model.fileDiff else {
            Issue.record("expected failure, got \(model.fileDiff)")
            return
        }
        #expect(change.path == "b.txt")
        #expect(message.contains("read blob"))
    }

    @Test func switchingRefsClearsTheCommitDetail() async {
        await model.attach(repo)
        await model.loadCommitChanges(commitSummary(mainCommit, parents: [parentCommit]))
        #expect(model.diffCommit != nil)
        await model.select(model.refs.refs[0])
        #expect(model.diffCommit == nil)
        #expect(model.commitChanges.isEmpty)
    }

    @Test func blameNeedsAnOpenFileWithAKnownPath() async {
        await model.attach(repo)
        do {
            _ = try await model.blameOpenedFile()
            Issue.record("expected a throw")
        } catch let error as RepositoryError {
            #expect(error.message.contains("no file"))
        } catch {
            Issue.record("unexpected error \(error)")
        }

        let entry = TreeEntry(name: "a.txt", oid: newBlob, kind: .blob)
        await model.openFile(entry, path: "a.txt")
        let spans = (try? await model.blameOpenedFile()) ?? []
        #expect(spans.count == 1)
        #expect(spans[0].commit.title == "parent")
        #expect(spans[0].range.lineCount == 1)
    }
}
