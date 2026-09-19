import Testing

@testable import RcGitKit

@Suite struct TextDocumentTests {
    @Test func decodesUtf8AndCountsLines() {
        let document = TextDocument(
            bytes: Array("a\nb\n".utf8), lineStarts: [0, 2], validUTF8: true)
        #expect(document.text == "a\nb\n")
        #expect(document.lineCount == 2)
        #expect(document.byteCount == 4)
        #expect(!document.isLossy)
    }

    @Test func replacesInvalidUtf8AndFlagsIt() {
        let document = TextDocument(bytes: [0x66, 0xff, 0x6f], lineStarts: [0], validUTF8: false)
        #expect(document.text == "f\u{fffd}o")
        #expect(document.isLossy)
    }

    @Test func onlyTextBlobsBecomeDocuments() {
        #expect(TextDocument(content: .binary(size: 3)) == nil)
        #expect(TextDocument(content: .tooLarge(size: 9, limit: 3)) == nil)
        #expect(TextDocument(content: textBlob("x"))?.lineCount == 1)
    }

    @Test func utf16LineStartsFollowTheCoreRule() {
        #expect(TextDocument(content: textBlob(""))?.utf16LineStarts == [])
        #expect(TextDocument(content: textBlob("a"))?.utf16LineStarts == [0])
        #expect(TextDocument(content: textBlob("a\n"))?.utf16LineStarts == [0])
        #expect(TextDocument(content: textBlob("a\nb"))?.utf16LineStarts == [0, 2])
        #expect(TextDocument(content: textBlob("a\nb\n"))?.utf16LineStarts == [0, 2])
        #expect(TextDocument(content: textBlob("\n\n"))?.utf16LineStarts == [0, 1])
    }

    @Test func utf16LineStartsCountUnitsNotBytes() {
        // "é" is 2 UTF-8 bytes but 1 UTF-16 unit; "😀" is 4 UTF-8 bytes and 2 UTF-16 units.
        let document = TextDocument(content: textBlob("é\n😀\nx"))
        #expect(document?.utf16LineStarts == [0, 2, 5])
    }

    @Test func byteFormatUsesShortUnits() {
        #expect(ByteFormat.string(0) == "0 B")
        #expect(ByteFormat.string(999) == "999 B")
        #expect(ByteFormat.string(1_000) == "1.0 KB")
        #expect(ByteFormat.string(12_345) == "12 KB")
        #expect(ByteFormat.string(1_234_567) == "1.2 MB")
        #expect(ByteFormat.string(5_000_000_000) == "5.0 GB")
    }
}
