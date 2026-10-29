import Foundation
import Testing

@testable import RcGitKit

/// One matching line at `line`, the query found at byte `column`.
private func hitLine(_ line: Int, _ text: String = "match", column: Int = 0) -> ContentLineMatch {
    ContentLineMatch(
        line: line, column: column, matchLength: 5, excerptBytes: Array(text.utf8),
        ranges: [0..<min(5, text.utf8.count)])
}

/// The pure parts: excerpt highlights, the status line, and match positions in a document.
@Suite(.timeLimit(.minutes(1))) struct ContentSearchTextTests {
    @Test func highlightsMapBytesToUTF16() {
        #expect(ContentLineMatch.utf16Ranges(of: [2..<5], in: Array("a needle".utf8)) == [2..<5])
        // é is two bytes and one unit; 😀 is four bytes and two units.
        #expect(ContentLineMatch.utf16Ranges(of: [3..<6], in: Array("é x yz".utf8)) == [2..<5])
        #expect(ContentLineMatch.utf16Ranges(of: [5..<6], in: Array("😀 x".utf8)) == [3..<4])
    }

    @Test func onlyAMatchRevealNeedsTheSource() {
        #expect(FileReveal.match(line: 0, byteColumn: 0, byteLength: 1).needsSource)
        #expect(!FileReveal.fragment("usage").needsSource)
    }

    @Test func lossyExcerptsAndRangesOutsideThemGetNoHighlight() {
        #expect(ContentLineMatch.utf16Ranges(of: [0..<1], in: [0x61, 0xFF, 0x62]).isEmpty)
        #expect(ContentLineMatch.utf16Ranges(of: [2..<9], in: Array("abc".utf8)).isEmpty)
        let lossy = ContentLineMatch(
            line: 0, column: 0, matchLength: 1, excerptBytes: [0x61, 0xFF], ranges: [0..<1])
        #expect(lossy.excerpt == "a\u{FFFD}")
        #expect(lossy.highlights.isEmpty)
    }

    @Test func statusSaysHowFarASearchGot() {
        var progress = ContentSearchProgress(totalFiles: 0, indexTruncated: false)
        #expect(ContentSearch.statusText(progress, finished: false) == "Searching…")
        progress.totalFiles = 2_500
        progress.searchedFiles = 1_000
        #expect(
            ContentSearch.statusText(progress, finished: false) == "Searching 1,000 of 2,500 files…"
        )
        #expect(ContentSearch.statusText(progress, finished: true) == "No matches")
    }

    @Test func finishedStatusNamesTotalsAndWhatWasLeftOut() {
        var progress = ContentSearchProgress(totalFiles: 10, indexTruncated: true)
        progress.matchedLines = 1
        progress.matchedFiles = 1
        progress.stoppedAtLimit = true
        progress.skippedFiles = 2
        progress.unreadableFiles = 1
        #expect(
            ContentSearch.statusText(progress, finished: true)
                == "1 line in 1 file · stopped at 2,000 lines"
                + " · only the first 200,000 entries searched"
                + " · 2 files binary or over 1 MB skipped · 1 file unreadable")
        progress = ContentSearchProgress(totalFiles: 10, indexTruncated: false)
        progress.matchedLines = 3
        progress.matchedFiles = 2
        #expect(ContentSearch.statusText(progress, finished: true) == "3 lines in 2 files")
    }

    @Test func aMatchMapsToItsUTF16RangeInTheDocument() {
        let text = "first\nsé needle\n😀needle\n"
        let document = TextDocument(
            bytes: Array(text.utf8), lineStarts: [0, 6, 17], validUTF8: true)
        #expect(document.utf16Range(line: 0, byteColumn: 0, byteLength: 5) == 0..<5)
        #expect(document.utf16Range(line: 1, byteColumn: 4, byteLength: 6) == 9..<15)
        #expect(document.utf16Range(line: 2, byteColumn: 4, byteLength: 6) == 18..<24)
        #expect(document.utf16Range(line: 3, byteColumn: 0, byteLength: 1) == nil)
        #expect(document.utf16Range(line: 0, byteColumn: 99, byteLength: 1) == 0..<0)
    }

    @Test func aMatchInLossyTextLandsAtItsLineStart() {
        let bytes: [UInt8] = Array("a\n".utf8) + [0xFF] + Array(" x".utf8)
        let document = TextDocument(bytes: bytes, lineStarts: [0, 2], validUTF8: false)
        #expect(document.utf16Range(line: 1, byteColumn: 2, byteLength: 1) == 2..<2)
    }
}

/// `BrowserModel.searchContent` over a fake repository: which files it reads, how it batches,
/// and when it stops.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct ContentSearchModelTests {
    let repo = FakeRepository()
    let model = BrowserModel()

    let commit = oid(0x11)
    let tree = oid(0x12)
    let srcTree = oid(0x30)
    let shared = oid(0x40)
    let other = oid(0x41)
    let hidden = oid(0x42)

    init() {
        repo.listing = RefListing(refs: [branch("main", at: commit)])
        repo.headValue = Head(refName: "refs/heads/main", target: commit)
        repo.treeOfMap = [commit: tree]
        repo.trees[tree] = [
            TreeEntry(name: "src", oid: srcTree, kind: .tree),
            TreeEntry(name: "README.md", oid: shared, kind: .blob),
            TreeEntry(name: "link", oid: other, kind: .symlink),
            TreeEntry(name: ".DS_Store", oid: hidden, kind: .blob),
        ]
        repo.trees[srcTree] = [
            TreeEntry(name: "copy.md", oid: shared, kind: .blob),
            TreeEntry(name: "main.rs", oid: other, kind: .blob),
        ]
    }

    /// Runs `query` and collects every reported hit's path, in report order.
    func search(_ query: String) async throws -> (paths: [String], progress: ContentSearchProgress)
    {
        var paths: [String] = []
        let progress = try await model.searchContent(query) { hits, _ in
            paths += hits.map(\.entry.path)
        }
        return (paths, progress)
    }

    @Test func eachBlobIsReadOnceAndHitsArriveInFileOrder() async throws {
        await model.attach(repo)
        repo.blobSearches[shared] = .searched(lines: [hitLine(0)], truncated: false)
        let (paths, progress) = try await search("match")
        #expect(paths == ["src/copy.md", "README.md"])
        #expect(
            repo.lock.withLock { repo.searchBlobCalls } == [[shared, other]],
            "two paths share one blob; the symlink and the hidden file are left out")
        #expect(progress.searchedFiles == 3)
        #expect(progress.matchedFiles == 2)
        #expect(progress.matchedLines == 2)
    }

    @Test func hiddenFilesAreSearchedWhileShown() async throws {
        await model.attach(repo)
        model.setShowsHiddenFiles(true)
        let (_, progress) = try await search("match")
        #expect(repo.lock.withLock { repo.searchBlobCalls } == [[shared, other, hidden]])
        #expect(progress.totalFiles == 4, ".DS_Store is in the list")
    }

    @Test func aCallThatStopsAtItsBudgetIsResumedWithTheRest() async throws {
        await model.attach(repo)
        repo.contentFilesPerCall = 1
        repo.blobSearches[other] = .searched(lines: [hitLine(3)], truncated: false)
        let (paths, _) = try await search("match")
        #expect(paths == ["src/main.rs"])
        #expect(repo.lock.withLock { repo.searchBlobCalls } == [[shared, other], [other]])
    }

    @Test func aCallThatReadsNothingFailsInsteadOfLooping() async throws {
        await model.attach(repo)
        repo.contentFilesPerCall = 0
        await #expect(throws: RepositoryError.self) { _ = try await search("match") }
        #expect(repo.lock.withLock { repo.searchBlobCalls.count } == 1)
    }

    @Test func theSearchStopsAtTheLineCap() async throws {
        await model.attach(repo)
        repo.contentFilesPerCall = 1
        let lines = (0..<ContentSearch.maxMatchedLines).map { hitLine($0) }
        repo.blobSearches[shared] = .searched(lines: lines, truncated: true)
        let (paths, progress) = try await search("match")
        #expect(paths == ["src/copy.md"], "the file that reaches the cap is the last reported")
        #expect(progress.stoppedAtLimit)
        #expect(repo.lock.withLock { repo.searchBlobCalls.count } == 1, "nothing more is read")
    }

    @Test func skippedAndUnreadableFilesAreCounted() async throws {
        repo.workDir = "/checkout"
        repo.worktreeFileList = WorktreeFileList(
            files: [
                WorktreeFile(path: "a.bin", kind: .blob),
                WorktreeFile(path: "big.txt", kind: .blob),
                WorktreeFile(path: "gone.txt", kind: .blob),
                WorktreeFile(path: "link", kind: .symlink),
            ], scanned: 4, truncated: true)
        await model.attach(repo)
        repo.worktreeSearches = [
            "a.bin": .binary, "big.txt": .tooLarge(size: 9, limit: 1),
            "gone.txt": .unreadable("missing"),
        ]
        let (paths, progress) = try await search("match")
        #expect(paths.isEmpty)
        #expect(
            repo.lock.withLock { repo.searchWorktreeCalls } == [["a.bin", "big.txt", "gone.txt"]])
        #expect(progress.skippedFiles == 2)
        #expect(progress.unreadableFiles == 1)
        #expect(progress.indexTruncated)
        #expect(repo.lock.withLock { repo.searchBlobCalls }.isEmpty, "disk, not blobs")
    }

    @Test func aNewerSearchSupersedesTheRunningOne() async throws {
        await model.attach(repo)
        repo.blobSearches[shared] = .searched(lines: [hitLine(0)], truncated: false)
        let gate = CallGate()
        repo.contentSearchGate = gate
        var staleReports = 0
        let first = Task {
            try await model.searchContent("old") { _, _ in staleReports += 1 }
        }
        await gate.waitUntilArrived()
        let newer = Task { try await search("new") }
        gate.open()
        await #expect(throws: CancellationError.self) { _ = try await first.value }
        let (paths, _) = try await newer.value
        #expect(paths == ["src/copy.md", "README.md"])
        #expect(staleReports == 0, "the superseded search reports nothing")
    }

    @Test func openingAMatchHandsThePaneItsPosition() async throws {
        await model.attach(repo)
        repo.blobs[shared] = textBlob("one\ntwo match\n")
        let entry = PathIndexEntry(path: "README.md", oid: shared, kind: .blob, hidden: false)
        await model.openContentMatch(entry, match: hitLine(1, column: 4))
        #expect(model.openedFilePath == "README.md")
        #expect(model.pendingReveal == .match(line: 1, byteColumn: 4, byteLength: 5))
        await model.openContentMatch(entry, match: nil)
        #expect(model.pendingReveal == nil, "a file opened at the top scrolls nowhere")
    }
}

/// The panel's session: when Return searches and when it opens, and which results stand.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct ContentSearchSessionTests {
    let session = ContentSearchSession()
    let entry = PathIndexEntry(path: "a.txt", oid: oid(1), kind: .blob, hidden: false)

    func hit(_ lines: Int) -> ContentSearchHit {
        ContentSearchHit(entry: entry, lines: (0..<lines).map { hitLine($0) }, truncated: false)
    }

    func answering(_ hits: [ContentSearchHit]) -> ContentSearchSession.Provider {
        { _, report in
            var progress = ContentSearchProgress(totalFiles: 1, indexTruncated: false)
            progress.searchedFiles = 1
            report(hits, progress)
            return progress
        }
    }

    @Test func returnSearchesANewQueryAndOpensTheSelectionOfTheOneOnScreen() async {
        session.provider = answering([hit(2)])
        var opened: [ContentLineMatch?] = []
        session.onOpen = { _, match in opened.append(match) }
        await session.submit("match", selectedRow: nil)?.value
        #expect(session.rows == [.file(0), .line(file: 0, line: 0), .line(file: 0, line: 1)])
        guard case .finished = session.status else {
            Issue.record("expected a finished search, got \(session.status)")
            return
        }
        #expect(session.submit("match", selectedRow: 2) == nil, "the same query opens")
        #expect(opened == [hitLine(1)])
        session.open(row: 0)
        #expect(opened == [hitLine(1), hitLine(0)], "a file row opens at its first match")
        session.open(row: 9)
        #expect(opened.count == 2, "a row past the list opens nothing")
        #expect(session.submit("other", selectedRow: 2) != nil, "an edited query searches again")
    }

    @Test func anEmptyQueryClearsTheListWithoutSearching() async {
        var calls = 0
        session.provider = { _, _ in
            calls += 1
            return ContentSearchProgress(totalFiles: 0, indexTruncated: false)
        }
        #expect(session.search("") == nil)
        #expect(calls == 0)
        #expect(session.status == .idle)
    }

    @Test func aFailedSearchSaysWhy() async {
        session.provider = { _, _ in throw RepositoryError("read blob: missing") }
        await session.search("x")?.value
        #expect(session.status == .failed("read blob: missing"))
    }

    @Test func onlyTheNewestQueryFillsTheList() async {
        let gate = OneShot<Void>()
        session.provider = { query, report in
            if query == "old" { await gate.value() }
            report(
                [
                    ContentSearchHit(
                        entry: entry, lines: [hitLine(query == "old" ? 7 : 1)],
                        truncated: false)
                ], ContentSearchProgress(totalFiles: 1, indexTruncated: false))
            return ContentSearchProgress(totalFiles: 1, indexTruncated: false)
        }
        let old = session.search("old")
        await session.search("new")?.value
        gate.fulfil(())
        await old?.value
        #expect(session.hits.map { $0.lines.map(\.line) } == [[1]])
        #expect(session.searchedQuery == "new")
    }
}
