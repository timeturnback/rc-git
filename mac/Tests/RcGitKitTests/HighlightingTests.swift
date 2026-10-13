import Foundation
import Testing

@testable import RcGitKit

/// The highlighting bridge and the byte-to-UTF-16 span mapping.
@Suite struct HighlightingTests {
    @Test func rustSourceGetsSpansAndUnknownExtensionsGetNone() {
        let data = Array("fn main() { let s = \"hi\"; }\n".utf8)
        let spans = Highlighter.highlight(name: "main.rs", data: data)
        #expect(spans != nil)
        #expect(spans?.isEmpty == false)
        #expect(spans?.contains { $0.class == .keyword } == true)
        #expect(spans?.contains { $0.class == .string } == true)

        #expect(Highlighter.highlight(name: "photo.png", data: data) == nil)
        #expect(Highlighter.highlight(name: "noext", data: data) == nil)
    }

    @Test func aFailureMessageIsLoggedOnce() {
        let message = "highlighting test-\(UUID()): the grammar failed to load: query"
        #expect(HighlightFailureLog.isFirst(message))
        #expect(!HighlightFailureLog.isFirst(message))
        #expect(HighlightFailureLog.isFirst(message + " again"))
    }

    @Test func oversizedInputStaysPlain() {
        let big = [UInt8](repeating: UInt8(ascii: "a"), count: Highlighter.byteLimit + 1)
        #expect(Highlighter.highlight(name: "big.rs", data: big) == nil)

        let atLimit = [UInt8](repeating: UInt8(ascii: "a"), count: Highlighter.byteLimit)
        #expect(Highlighter.highlight(name: "big.rs", data: atLimit) != nil)
    }

    @Test func mapperConvertsAsciiOffsetsOneToOne() throws {
        let text = "fn main() {}"
        let spans = [
            HighlightSpan(start: 0, end: 2, class: .keyword),
            HighlightSpan(start: 3, end: 7, class: .function),
        ]
        let ranges = HighlightMapper.utf16Ranges(for: spans, in: text)
        try #require(ranges.count == 2)
        #expect(ranges[0].range == NSRange(location: 0, length: 2))
        #expect(ranges[0].class == .keyword)
        #expect(ranges[1].range == NSRange(location: 3, length: 4))
    }

    @Test func mapperAccountsForMultibyteCharacters() throws {
        // "é" is 2 UTF-8 bytes, 1 UTF-16 unit; "🙂" is 4 UTF-8 bytes, 2 UTF-16 units.
        let text = "é🙂x = 1"
        let bytes = Array(text.utf8)
        #expect(bytes.count == 11)
        // Highlight the "x" (byte 6) and the "1" (byte 10).
        let spans = [
            HighlightSpan(start: 6, end: 7, class: .variable),
            HighlightSpan(start: 10, end: 11, class: .number),
        ]
        let ranges = HighlightMapper.utf16Ranges(for: spans, in: text)
        try #require(ranges.count == 2)
        #expect(ranges[0].range == NSRange(location: 3, length: 1), "é=1, 🙂=2 UTF-16 units")
        #expect(ranges[1].range == NSRange(location: 7, length: 1))
    }

    @Test func mapperDropsEmptyAndHandlesEndOfText() throws {
        let text = "abc"
        let spans = [
            HighlightSpan(start: 1, end: 1, class: .keyword),
            HighlightSpan(start: 2, end: 3, class: .string),
        ]
        let ranges = HighlightMapper.utf16Ranges(for: spans, in: text)
        try #require(ranges.count == 1, "zero-length spans are dropped")
        #expect(ranges[0].range == NSRange(location: 2, length: 1))
        #expect(HighlightMapper.utf16Ranges(for: [], in: "x").isEmpty)
        #expect(HighlightMapper.utf16Ranges(for: spans, in: "").isEmpty)
    }
}

/// The model-level cache: one parse per blob id.
@MainActor
@Suite(.timeLimit(.minutes(1))) struct ModelHighlightTests {
    @Test func highlightsAreCachedByBlobAndSkipLossyText() async {
        let model = BrowserModel()
        let repo = FakeRepository()
        await model.attach(repo)

        let entry = TreeEntry(name: "main.rs", oid: oid(0x42), kind: .blob)
        let document = TextDocument(
            bytes: Array("fn main() {}\n".utf8), lineStarts: [0], validUTF8: true)
        let first = await model.highlights(for: entry, document: document)
        #expect(first?.isEmpty == false)
        // Same blob id, different text: only a cache hit answers with the first parse.
        let other = TextDocument(bytes: Array("x\n".utf8), lineStarts: [0], validUTF8: true)
        #expect(await model.highlights(for: entry, document: other) == first)

        let lossy = TextDocument(bytes: [0x66, 0xFF, 0x6F], lineStarts: [0], validUTF8: false)
        #expect(lossy.isLossy)
        let none = await model.highlights(
            for: TreeEntry(name: "bad.rs", oid: oid(0x43), kind: .blob), document: lossy)
        #expect(none == nil, "misaligned offsets are worse than plain text")
    }

    @Test func aBlobUnderAnotherExtensionIsHighlightedForThatName() async {
        let model = BrowserModel()
        await model.attach(FakeRepository())

        let document = TextDocument(
            bytes: Array("fn f() {}\n".utf8), lineStarts: [0], validUTF8: true)
        let rust = await model.highlights(
            for: TreeEntry(name: "a.rs", oid: oid(0x51), kind: .blob), document: document)
        #expect(rust?.isEmpty == false)
        let backup = await model.highlights(
            for: TreeEntry(name: "a.bak", oid: oid(0x51), kind: .blob), document: document)
        #expect(backup == nil, "the same blob under an unknown extension has no language")
        let upper = await model.highlights(
            for: TreeEntry(name: "A.RS", oid: oid(0x51), kind: .blob), document: document)
        #expect(upper == rust)
    }

    @Test func markdownHighlightsCrossTheBridge() {
        let data = Array("# Title\n\n```rust\nfn f() {}\n```\n".utf8)
        let spans = Highlighter.highlight(name: "README.md", data: data)
        #expect(spans?.contains { $0.class == .heading } == true)
        #expect(spans?.contains { $0.class == .keyword } == true, "fenced rust keeps its color")
    }

    @Test func spanCacheEvictsLeastRecentlyUsedPastTheEntryCap() async {
        let model = BrowserModel()
        await model.attach(FakeRepository())
        let limit = model.highlightCacheLimit
        #expect(spansA != spansB(seed: 1), "the probe tells a hit from a miss")

        for seed in 1...limit + 1 {
            #expect(await model.highlights(for: rust(seed), document: plainA)?.isEmpty == false)
        }
        // A miss parses the new text: the oldest entry was evicted.
        #expect(await model.highlights(for: rust(1), document: plainB) == spansB(seed: 1))
        // A hit answers with the first parse: the newest entry stayed.
        #expect(await model.highlights(for: rust(limit + 1), document: plainB) == spansA)
    }

    @Test func aCacheHitMovesTheEntryToTheRecentEnd() async {
        let model = BrowserModel()
        await model.attach(FakeRepository())
        let limit = model.highlightCacheLimit

        for seed in 1...limit {
            _ = await model.highlights(for: rust(seed), document: plainA)
        }
        _ = await model.highlights(for: rust(1), document: plainA)
        _ = await model.highlights(for: rust(limit + 1), document: plainA)
        #expect(await model.highlights(for: rust(1), document: plainB) == spansA, "touched")
        #expect(
            await model.highlights(for: rust(2), document: plainB) == spansB(seed: 2),
            "the least recently used entry goes instead")
    }

    @Test func spanCacheEvictsPastTheSpanCapButKeepsOneEntry() async throws {
        let model = BrowserModel(highlightCacheLimit: 16, highlightCacheSpanLimit: 10)
        await model.attach(FakeRepository())
        let small = TextDocument(bytes: Array("fn f() {}\n".utf8), lineStarts: [0], validUTF8: true)
        let smallSpans = try #require(await model.highlights(for: rust(1), document: small))
        #expect(smallSpans.count > 5 && smallSpans.count <= 10, "two entries overflow the cap")

        _ = await model.highlights(for: rust(2), document: small)
        #expect(await model.highlights(for: rust(2), document: plainB) == smallSpans)
        #expect(await model.highlights(for: rust(1), document: plainB) == spansB(seed: 1))

        let tiny = BrowserModel(highlightCacheLimit: 16, highlightCacheSpanLimit: 1)
        await tiny.attach(FakeRepository())
        _ = await tiny.highlights(for: rust(1), document: small)
        #expect(
            await tiny.highlights(for: rust(1), document: plainB) == smallSpans,
            "a single entry over the cap is still cached")
    }

    @Test func unhashedWorkingTreeFilesEachGetTheirOwnSpans() async throws {
        let model = BrowserModel()
        await model.attach(FakeRepository())

        let shortBytes = Array("fn a() {}\n".utf8)
        let longBytes = Array("// note\nfn b() { let s = \"hi\"; }\n".utf8)
        let short = TextDocument(bytes: shortBytes, lineStarts: [0], validUTF8: true)
        let long = TextDocument(bytes: longBytes, lineStarts: [0, 8], validUTF8: true)
        let first = try #require(
            await model.highlights(
                for: TreeEntry(name: "a.rs", oid: .unhashed, kind: .blob), document: short))
        let second = try #require(
            await model.highlights(
                for: TreeEntry(name: "b.rs", oid: .unhashed, kind: .blob), document: long))
        #expect(first == Highlighter.highlight(name: "a.rs", data: shortBytes))
        #expect(second == Highlighter.highlight(name: "b.rs", data: longBytes))
        #expect(first != second, "two disk files must not share one cache slot")
    }

    private let plainA = TextDocument(
        bytes: Array("fn main() {}\n".utf8), lineStarts: [0], validUTF8: true)
    private let plainB = TextDocument(
        bytes: Array("let s = \"hi\";\n".utf8), lineStarts: [0], validUTF8: true)
    private var spansA: [HighlightSpan]? {
        Highlighter.highlight(name: "f.rs", data: Array(plainA.text.utf8))
    }

    private func spansB(seed: Int) -> [HighlightSpan]? {
        Highlighter.highlight(name: rust(seed).name, data: Array(plainB.text.utf8))
    }

    private func rust(_ seed: Int) -> TreeEntry {
        TreeEntry(name: "f\(seed).rs", oid: oid(UInt8(seed)), kind: .blob)
    }
}
