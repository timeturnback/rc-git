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

    @Test func oversizedInputStaysPlain() {
        let big = [UInt8](repeating: UInt8(ascii: "a"), count: Highlighter.byteLimit + 1)
        #expect(Highlighter.highlight(name: "big.rs", data: big) == nil)
    }

    @Test func mapperConvertsAsciiOffsetsOneToOne() {
        let text = "fn main() {}"
        let spans = [
            HighlightSpan(start: 0, end: 2, class: .keyword),
            HighlightSpan(start: 3, end: 7, class: .function),
        ]
        let ranges = HighlightMapper.utf16Ranges(for: spans, in: text)
        #expect(ranges.count == 2)
        #expect(ranges[0].range == NSRange(location: 0, length: 2))
        #expect(ranges[0].class == .keyword)
        #expect(ranges[1].range == NSRange(location: 3, length: 4))
    }

    @Test func mapperAccountsForMultibyteCharacters() {
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
        #expect(ranges.count == 2)
        #expect(ranges[0].range == NSRange(location: 3, length: 1), "é=1, 🙂=2 UTF-16 units")
        #expect(ranges[1].range == NSRange(location: 7, length: 1))
    }

    @Test func mapperDropsEmptyAndHandlesEndOfText() {
        let text = "abc"
        let spans = [
            HighlightSpan(start: 1, end: 1, class: .keyword),
            HighlightSpan(start: 2, end: 3, class: .string),
        ]
        let ranges = HighlightMapper.utf16Ranges(for: spans, in: text)
        #expect(ranges.count == 1, "zero-length spans are dropped")
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
        let second = await model.highlights(for: entry, document: document)
        #expect(first == second)

        let lossy = TextDocument(bytes: [0x66, 0xFF, 0x6F], lineStarts: [0], validUTF8: false)
        #expect(lossy.isLossy)
        let none = await model.highlights(
            for: TreeEntry(name: "bad.rs", oid: oid(0x43), kind: .blob), document: lossy)
        #expect(none == nil, "misaligned offsets are worse than plain text")
    }

    @Test func markdownHighlightsCrossTheBridge() {
        let data = Array("# Title\n\n```rust\nfn f() {}\n```\n".utf8)
        let spans = Highlighter.highlight(name: "README.md", data: data)
        #expect(spans?.contains { $0.class == .heading } == true)
        #expect(spans?.contains { $0.class == .keyword } == true, "fenced rust keeps its color")
    }

    @Test func spanCacheEvictsLeastRecentlyUsedPastTheEntryCap() async {
        let model = BrowserModel()
        let repo = FakeRepository()
        await model.attach(repo)

        let document = TextDocument(
            bytes: Array("fn main() {}\n".utf8), lineStarts: [0], validUTF8: true)
        for seed in 0...BrowserModel.highlightCacheLimit {
            let entry = TreeEntry(name: "f\(seed).rs", oid: oid(UInt8(seed + 1)), kind: .blob)
            let spans = await model.highlights(for: entry, document: document)
            #expect(spans?.isEmpty == false)
        }
        // One over the cap: the first entry was evicted, everything still answers.
        let again = await model.highlights(
            for: TreeEntry(name: "f0.rs", oid: oid(1), kind: .blob), document: document)
        #expect(again?.isEmpty == false)
    }
}
