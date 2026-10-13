import Foundation
import Testing

@testable import RcGitKit

/// History paging, commit browsing, and the refs refresh driven by the file watcher.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct BrowserHistoryTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let featureCommit = oid(0x21)
    let featureTree = oid(0x22)

    init() {
        repo.listing = RefListing(refs: [
            branch("feature", at: featureCommit),
            branch("main", at: mainCommit),
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

    @Test func pathScopeFiltersAndRestores() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        #expect(model.history.count == 100)
        let touching = [
            commitSummary(oid(0xA1), title: "touch 1"), commitSummary(oid(0xA2), title: "touch 2"),
        ]
        repo.lock.withLock {
            repo.pathHistories[FakeRepository.pathKey(tip: mainCommit, path: "src/a.rs")] =
                touching
        }
        await model.showPathHistory("src/a.rs")
        #expect(model.historyScope == .path("src/a.rs"))
        #expect(model.history.map(\.title) == ["touch 1", "touch 2"])
        #expect(model.historyComplete)
        model.clearHistoryScope()
        #expect(model.historyScope == .all)
        #expect(model.history.count == 100, "the unfiltered pages come back from the cache")
    }

    @Test func pathScopeCachesPerTipAndPath() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        repo.lock.withLock {
            repo.pathHistories[FakeRepository.pathKey(tip: mainCommit, path: "a")] = [
                commitSummary(oid(0xA1), title: "touch a")
            ]
            repo.pathHistories[FakeRepository.pathKey(tip: mainCommit, path: "b")] = [
                commitSummary(oid(0xB1), title: "touch b")
            ]
        }
        await model.showPathHistory("a")
        #expect(model.history.map(\.title) == ["touch a"])
        await model.showPathHistory("b")
        #expect(model.history.map(\.title) == ["touch b"])
        let calls = repo.lock.withLock { repo.pathHistoryCalls }
        await model.showPathHistory("a")
        #expect(model.history.map(\.title) == ["touch a"])
        #expect(
            repo.lock.withLock { repo.pathHistoryCalls } == calls,
            "a complete scope comes back without a walk")
    }

    @Test func pathScopeWalksInChunksAndScopeChangeDropsTheRest() async {
        await model.attach(repo)
        let touching = (0..<150).map { index in
            commitSummary(oid(UInt8(index % 250)), title: "touch \(index)")
        }
        repo.lock.withLock {
            repo.pathHistories[FakeRepository.pathKey(tip: mainCommit, path: "big")] = touching
        }
        await model.showPathHistory("big")
        #expect(model.history.count == 100, "one page even though more commits match")
        #expect(!model.historyComplete)
        await model.loadMoreHistory()
        #expect(model.history.count == 150)
        #expect(model.historyComplete)
    }

    @Test func graphRowsFollowThePagedHistoryAndItsCache() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        #expect(model.graphRows.count == 100, "one graph row per paged commit")
        await model.loadMoreHistory()
        #expect(model.graphRows.count == 150)

        await model.select(model.refs.refs[0])
        #expect(model.graphRows.count <= 1, "another tip starts its own graph")
        await model.select(model.refs.refs[1])
        #expect(model.graphRows.count == 150, "the cached graph returns with the pages")

        await model.search(CommitFilter(message: "main"))
        #expect(model.graphRows.isEmpty, "a filtered list has no topology")
        model.clearSearch()
        #expect(model.graphRows.count == 150)
    }

    @Test func searchStreamsMatchesAndClearRestoresInstantly() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        var events = 0
        model.onSearchChanged = { events += 1 }
        await model.search(CommitFilter(message: "main 1"))
        #expect(model.searchResults.count == 61, "main 1, main 10-19, main 100-149")
        #expect(model.searchComplete)
        #expect(!model.isSearching)
        #expect(events >= 2, "started, then streamed")

        let calls = repo.lock.withLock { repo.searchCalls }
        model.clearSearch()
        #expect(model.searchFilter == nil)
        #expect(model.searchResults.isEmpty)
        #expect(
            repo.lock.withLock { repo.searchCalls } == calls,
            "clearing needs no repository call")
        #expect(model.history.count == 100, "the paged history is untouched")
    }

    @Test func searchPagesOnDemand() async {
        await model.attach(repo)
        await model.search(CommitFilter(message: "main"))
        #expect(model.searchResults.count == 100, "one page of matches")
        #expect(!model.searchComplete)
        await model.loadMoreSearchResults()
        #expect(model.searchResults.count == 150)
        #expect(model.searchComplete)
    }

    @Test func refSwitchAndEmptyFilterClearTheSearch() async {
        await model.attach(repo)
        await model.search(CommitFilter(message: "main"))
        #expect(model.searchFilter != nil)
        await model.select(model.refs.refs[0])
        #expect(model.searchFilter == nil)
        #expect(model.searchResults.isEmpty)

        await model.search(CommitFilter(message: "feature"))
        #expect(model.searchFilter != nil)
        await model.search(CommitFilter())
        #expect(model.searchFilter == nil, "an empty filter clears instead of matching all")
    }

    @Test func pathScopeErrorSurfaces() async {
        await model.attach(repo)
        var reported: String?
        model.onError = { reported = $0 }
        await model.showPathHistory("unknown")
        #expect(reported?.contains("start commit walk") == true)
    }

    @Test func pathScopeFailureKeepsShownCommitsAndStopsPaging() async {
        await model.attach(repo)
        let touching = (0..<150).map { index in
            commitSummary(oid(UInt8(index % 250)), title: "touch \(index)")
        }
        repo.lock.withLock {
            repo.pathHistories[FakeRepository.pathKey(tip: mainCommit, path: "big")] = touching
        }
        await model.showPathHistory("big")
        #expect(model.history.count == 100)
        repo.lock.withLock { repo.pathHistoryFailFrom = 100 }
        var reports = 0
        model.onError = { _ in reports += 1 }
        await model.loadMoreHistory()
        #expect(model.history.count == 100, "the commits already shown stay")
        #expect(model.historyComplete, "a failed walk stops paging")
        #expect(!model.isLoadingHistory)
        let calls = repo.lock.withLock { repo.pathHistoryCalls }
        await model.loadMoreHistory()
        #expect(repo.lock.withLock { repo.pathHistoryCalls } == calls, "no re-walk")
        #expect(reports == 1, "one alert, not one per scroll")
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
            repo.listing.refs[1] = branch("main", at: featureCommit)
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

    @Test func refreshWithEveryRefGoneEmptiesTheBrowser() async {
        await model.attach(repo)
        await model.loadMoreHistory()
        #expect(model.history.count == 100)
        repo.lock.withLock {
            repo.listing = RefListing(refs: [])
            repo.headValue = Head(refName: nil, target: nil)
        }
        var rootEvents = 0
        model.onRootChanged = { rootEvents += 1 }
        await model.refreshRefs()
        #expect(model.selectedRef == nil)
        #expect(model.rootTree == nil, "the deleted ref's tree must not stay on screen")
        #expect(model.history.isEmpty, "the deleted ref's history must not stay on screen")
        #expect(rootEvents == 1)
    }

    @Test func refreshKeepsABrowsedCommit() async {
        await model.attach(repo)
        let commit = commitSummary(featureCommit, title: "feature 0")
        await model.browseCommit(commit)
        repo.lock.withLock {
            repo.listing.refs.append(
                RefEntry(
                    fullName: "refs/tags/v2", shortName: "v2", kind: .tag, target: mainCommit))
        }
        var rootEvents = 0
        model.onRootChanged = { rootEvents += 1 }
        await model.refreshRefs()
        #expect(model.refs.refs.count == 3)
        #expect(model.selectedRef == nil)
        #expect(model.browsedCommit == commit, "a fetch must not leave the browsed commit")
        #expect(model.rootTree == featureTree)
        #expect(rootEvents == 0)
    }

    @Test func refreshFailureKeepsTheLastGoodListingQuietly() async {
        await model.attach(repo)
        let failing = repo
        failing.lock.withLock { failing.failNextRefs = true }
        var reported: String?
        model.onError = { reported = $0 }
        var refsEvents = 0
        model.onRefsChanged = { refsEvents += 1 }
        await model.refreshRefs()
        #expect(reported == nil, "transient lock errors during a ref update must not alert")
        #expect(model.refsStale, "the kept listing must not read as current")
        #expect(refsEvents == 1, "the views re-render to show the stale state")
        #expect(model.refs.refs.count == 2)
        #expect(model.selectedRef?.shortName == "main")

        failing.lock.withLock {
            failing.listing.refs.append(
                RefEntry(
                    fullName: "refs/tags/v2", shortName: "v2", kind: .tag, target: mainCommit))
        }
        await model.refreshRefs()
        #expect(!model.refsStale, "a successful read clears the stale state")
        #expect(model.refs.refs.count == 3)
        #expect(model.selectedRef?.shortName == "main")
        #expect(reported == nil)
    }

    @Test func aBurstOfRefreshesCostsTwoListings() async {
        await model.attach(repo)
        repo.lock.withLock {
            repo.refsCalls = 0
            repo.refsDelay = 1
        }
        let slow = Task { await model.refreshRefs() }
        try? await Task.sleep(for: .milliseconds(20))
        repo.lock.withLock {
            repo.listing.refs.append(
                RefEntry(
                    fullName: "refs/tags/v2", shortName: "v2", kind: .tag, target: mainCommit))
        }
        let burst = (0..<5).map { _ in Task { await model.refreshRefs() } }
        await slow.value
        for refresh in burst { await refresh.value }
        #expect(repo.lock.withLock { repo.refsCalls } == 2, "the burst joins one more listing")
        #expect(model.refs.refs.count == 3, "the last listing read is the latest")
    }
}

/// Commit detail: changed files, one file's diff, and blame for the open file.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct BrowserDiffTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let mainCommit = oid(0x11)
    let mainTree = oid(0x12)
    let parentCommit = oid(0x10)
    let oldBlob = oid(0x41)
    let newBlob = oid(0x42)

    init() {
        repo.listing = RefListing(refs: [
            branch("main", at: mainCommit)
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
        repo.blames["a.txt"] = .ranges([
            BlameRange(commit: parentCommit, startLine: 1, lineCount: 1)
        ])
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

    @Test func jumpingToAParentShowsItsDetail() async {
        await model.attach(repo)
        repo.lock.withLock { repo.treeDiffs[parentCommit] = [] }
        await model.loadCommitChanges(commitSummary(mainCommit, parents: [parentCommit]))
        await model.jumpToCommit(parentCommit)
        #expect(model.diffCommit?.oid == parentCommit)
        guard case let .loaded(detail, _) = model.commitDetail else {
            Issue.record("expected a loaded detail, got \(model.commitDetail)")
            return
        }
        #expect(detail.summary.title == "parent")
    }

    @Test func jumpToRevisionResolvesAndShowsDetail() async {
        await model.attach(repo)
        repo.lock.withLock {
            repo.resolves["abc1"] = parentCommit
            repo.treeDiffs[parentCommit] = []
        }
        await model.jumpToRevision("abc1")
        #expect(model.diffCommit?.oid == parentCommit)

        var readFailure: String?
        var inputError: (title: String, message: String)?
        model.onError = { readFailure = $0 }
        model.onInputError = { inputError = ($0, $1) }
        await model.jumpToRevision("nope")
        #expect(inputError?.title == BrowserModel.revisionNotFoundTitle)
        #expect(inputError?.message.contains("not a revision") == true)
        #expect(readFailure == nil)
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

    @Test func aSubmoduleChangeNeverReachesDiffBlobs() async throws {
        await model.attach(repo)
        let oldCommit = try #require(Oid(hex: String(repeating: "1", count: 40)))
        let newCommit = try #require(Oid(hex: String(repeating: "2", count: 40)))
        let bump = TreeChange(
            kind: .modified, path: "sub", sourcePath: nil, old: oldCommit, new: newCommit,
            entry: .submodule)
        var events = 0
        model.onDiffChanged = { events += 1 }
        await model.loadFileDiff(bump)
        #expect(repo.lock.withLock { repo.diffBlobsCalls } == 0)
        guard case let .submodule(change) = model.fileDiff else {
            Issue.record("expected a submodule state, got \(model.fileDiff)")
            return
        }
        #expect(change == bump)
        #expect(events == 1)
        #expect(
            FileDiffState.submoduleSummary(change)
                == "Subproject commit \(oldCommit.hex) → \(newCommit.hex)")
    }

    @Test func closingTheCommitDetailEmptiesIt() async {
        await model.attach(repo)
        repo.lock.withLock { repo.treeDiffs[parentCommit] = [] }
        await model.loadCommitChanges(commitSummary(mainCommit, parents: [parentCommit]))
        await model.jumpToCommit(parentCommit)
        var events = 0
        model.onDiffChanged = { events += 1 }
        model.closeCommitDetail()
        #expect(model.diffCommit == nil)
        #expect(model.commitChanges.isEmpty)
        guard case .empty = model.commitDetail else {
            Issue.record("expected an empty detail after close, got \(model.commitDetail)")
            return
        }
        #expect(events == 1, "the view hears one change to dismiss the drawer")
        model.closeCommitDetail()
        #expect(events == 1, "closing an already-empty detail is silent")
    }

    @Test func switchingRefsClearsTheCommitDetail() async {
        await model.attach(repo)
        await model.loadCommitChanges(commitSummary(mainCommit, parents: [parentCommit]))
        #expect(model.diffCommit != nil)
        await model.select(model.refs.refs[0])
        #expect(model.diffCommit == nil)
        #expect(model.commitChanges.isEmpty)
        #expect(model.commitDetail == .empty)
    }

    @Test func blameNeedsAnOpenFileWithAKnownPath() async {
        await model.attach(repo)
        await model.requestBlame()
        #expect(model.blame == .failed("no file with a known path is open"))

        let entry = TreeEntry(name: "a.txt", oid: newBlob, kind: .blob)
        await model.openFile(entry, path: "a.txt")
        await model.requestBlame()
        guard case let .loaded(path, spans) = model.blame else {
            Issue.record("expected a loaded blame, got \(model.blame)")
            return
        }
        #expect(path == "a.txt")
        #expect(spans.count == 1)
        #expect(spans[0].commit.title == "parent")
        #expect(spans[0].range.lineCount == 1)
        #expect(spans[0].olderThan == nil, "a traced range is not older than the floor")
        #expect(repo.blameLimits == [Repository.defaultBlobLimit])
    }

    @Test func blameWalksTwoYearsBackFromTheTipAndMarksOlderLines() async throws {
        await model.attach(repo)
        let tipTime: Int64 = 1_700_000_000
        let detail = CommitDetail(
            summary: commitSummary(mainCommit), body: "", committerName: "Test Committer",
            committerEmail: "committer@example.com", committerTime: tipTime, committerOffset: 0,
            tree: mainTree)
        repo.lock.withLock { repo.details[mainCommit] = detail }
        repo.blames["a.txt"] = .ranges([
            BlameRange(commit: parentCommit, startLine: 1, lineCount: 1, boundary: true)
        ])
        let entry = TreeEntry(name: "a.txt", oid: newBlob, kind: .blob)
        await model.openFile(entry, path: "a.txt")
        await model.requestBlame()
        guard case let .loaded(_, spans) = model.blame else {
            Issue.record("expected a loaded blame, got \(model.blame)")
            return
        }
        let floor = tipTime - 730 * 86_400
        #expect(repo.blameFloors == [floor], "the floor is two years before the tip's commit")
        let span = try #require(spans.first)
        #expect(span.olderThan == floor, "a boundary range is shown as older than the floor")
    }

    @Test func blameUsesTheFilePaneLimitAndDeclinesAFileOverIt() async {
        await model.attach(repo)
        repo.blames["a.txt"] = .tooLarge(size: 40, limit: 10)
        let entry = TreeEntry(name: "a.txt", oid: newBlob, kind: .blob)
        await model.openFile(entry, path: "a.txt", limit: 10)
        await model.requestBlame()
        guard case let .failed(message) = model.blame else {
            Issue.record("expected a failed blame, got \(model.blame)")
            return
        }
        #expect(message.contains("40 bytes, over the 10 byte limit"))
        #expect(repo.blameLimits == [10])
    }
}
