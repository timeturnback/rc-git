import Testing

@testable import RcGitKit

/// The line a diff or file pane shows instead of content.
@Suite struct BlobDiffPlaceholderTests {
    @Test func identicalTextSaysNoLineChanges() {
        #expect(BlobDiff.text(hunks: []).placeholder(path: "a/b.txt") == "a/b.txt: no line changes")
    }

    @Test func textWithHunksHasNoPlaceholder() {
        let hunk = DiffHunk(
            oldStart: 1, oldLines: 0, newStart: 1, newLines: 1,
            lines: [DiffLine(kind: .added, text: "x")])
        #expect(BlobDiff.text(hunks: [hunk]).placeholder(path: "a.txt") == nil)
    }

    @Test func binaryNamesBothSizes() {
        let diff = BlobDiff.binary(oldSize: 999, newSize: 1_234_567)
        #expect(diff.placeholder(path: "logo.png") == "logo.png is binary (999 B → 1.2 MB)")
    }

    @Test func tooLargeNamesSizeAndLimit() {
        let diff = BlobDiff.tooLarge(size: 12_345_678, limit: 5_000_000)
        #expect(diff.placeholder(path: "dump.sql") == "dump.sql is 12 MB, over the 5.0 MB limit")
    }

    @Test func fileTooLargeUsesTheDiffWording() {
        let text = BlobDiff.tooLargePlaceholder(path: "big.bin", size: 2_000, limit: 1_000)
        #expect(text == "big.bin is 2.0 KB, over the 1.0 KB limit")
    }
}
