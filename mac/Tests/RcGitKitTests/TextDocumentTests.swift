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

    @Test func nonLFBreaksRecordEverySeparatorTextKitBreaksOn() {
        func breaks(_ text: String) -> [Int]? {
            TextDocument(content: textBlob(text))?.utf16NonLFBreaks
        }
        #expect(breaks("a\nb") == [])
        #expect(breaks("a\rb") == [1])
        #expect(breaks("a\r\nb") == [])
        #expect(breaks("a\rb\rc\r") == [1, 3, 5])
        #expect(breaks("a\u{0C}\nb") == [1])
        #expect(breaks("a\u{0B}b\u{85}c") == [1, 3])
        #expect(breaks("a\u{2028}b\u{2029}c") == [1, 3])
        #expect(breaks("") == [])
    }

    @Test func nonLFBreaksLeaveLineStartsOnLF() {
        let document = TextDocument(content: textBlob("😀\r\u{2028}x\ny"))
        #expect(document?.utf16LineStarts == [0, 6])
        #expect(document?.utf16NonLFBreaks == [2, 3])
    }

    @Test(arguments: [
        "plain ascii\nsecond line", "é\nñ ü", "日本\n語", "😀\n𝄞x", "", "a\n", "\n",
        "😀\r\u{2028}x\ny", "a\r\nb\rc\u{0B}d\u{0C}e\u{85}f\u{2029}g\r",
    ])
    func validUtf8BreaksMatchTheUtf16Walk(_ text: String) {
        let fromBytes = TextDocument.utf8Breaks(of: Array(text.utf8))
        let walked = TextDocument.utf16Breaks(of: text)
        #expect(fromBytes.lineStarts == walked.lineStarts)
        #expect(fromBytes.nonLF == walked.nonLF)
        #expect(TextDocument(content: textBlob(text))?.utf16LineStarts == walked.lineStarts)
    }

    @Test func lossyTextWalksTheDecodedText() {
        // U+FFFD is one unit standing in for one invalid byte here; a leading continuation
        // byte would count as zero units by the byte rule and put line 2 at 1.
        let document = TextDocument(
            bytes: [0x80, 0x0A, 0x61], lineStarts: [0, 2], validUTF8: false)
        #expect(document.utf16LineStarts == [0, 2])
    }

    @Test func byteFormatUsesShortUnits() {
        #expect(ByteFormat.string(0) == "0 B")
        #expect(ByteFormat.string(999) == "999 B")
        #expect(ByteFormat.string(1_000) == "1.0 KB")
        #expect(ByteFormat.string(12_345) == "12 KB")
        #expect(ByteFormat.string(1_234_567) == "1.2 MB")
        #expect(ByteFormat.string(5_000_000_000) == "5.0 GB")
    }

    @Test func byteFormatRoundsAcrossUnitBoundaries() {
        #expect(ByteFormat.string(9_960) == "10 KB")
        #expect(ByteFormat.string(9_999) == "10 KB")
        #expect(ByteFormat.string(999_499) == "999 KB")
        #expect(ByteFormat.string(999_500) == "1.0 MB")
        #expect(ByteFormat.string(999_999) == "1.0 MB")
        #expect(ByteFormat.string(999_999_999) == "1.0 GB")
        #expect(ByteFormat.string(999_999_999_999) == "1000 GB")
    }

    @Test func overLimitFallsBackToExactBytesWhenShortStringsMatch() {
        let limit: UInt64 = 8 * 1024 * 1024
        let justOver = ByteFormat.overLimit(size: limit + 1, limit: limit)
        #expect(justOver.size == "8,388,609 bytes")
        #expect(justOver.limit == "8,388,608-byte")
        let double = ByteFormat.overLimit(size: 2 * limit, limit: limit)
        #expect(double.size == "17 MB")
        #expect(double.limit == "8.4 MB")
        let text = BlobDiff.tooLargePlaceholder(path: "big.bin", size: limit + 1, limit: limit)
        #expect(text == "big.bin is 8,388,609 bytes, over the 8,388,608-byte limit")
    }
}

@Suite struct LineNumberingTests {
    @Test func labelsOnlyExactLineStarts() {
        // "a\nb\nc": LF lines start at 0, 2, 4.
        #expect(LineNumbering.label(at: 0, lineStarts: [0, 2, 4]) == 1)
        #expect(LineNumbering.label(at: 2, lineStarts: [0, 2, 4]) == 2)
        #expect(LineNumbering.label(at: 4, lineStarts: [0, 2, 4]) == 3)
        #expect(LineNumbering.label(at: 1, lineStarts: [0, 2, 4]) == nil)
        #expect(LineNumbering.label(at: 5, lineStarts: [0, 2, 4]) == nil)
        #expect(LineNumbering.label(at: 0, lineStarts: []) == nil)
    }

    @Test func followsTheDocumentsLineStarts() throws {
        func labels(_ text: String, at offsets: [Int]) throws -> [Int?] {
            let starts = try #require(TextDocument(content: textBlob(text))).utf16LineStarts
            return offsets.map { LineNumbering.label(at: $0, lineStarts: starts) }
        }
        // CRLF: the second row starts after the LF, at 3.
        #expect(try labels("a\r\nb", at: [0, 3]) == [1, 2])
        // CR only: TextKit would start rows at 2 and 4; neither is a line start.
        #expect(try labels("a\rb\rc", at: [0, 2, 4]) == [1, nil, nil])
        // U+2028 mid-line: the row after it is a continuation.
        #expect(try labels("a\u{2028}b", at: [0, 2]) == [1, nil])
        // FF before LF: the empty row at 2 is a continuation, the LF line starts at 3.
        #expect(try labels("a\u{0C}\nb", at: [0, 2, 3]) == [1, nil, 2])
    }
}
