import Testing

@testable import RcGitKit

/// What the file pane offers for a file: the markdown preview, and "Load anyway".
@Suite(.timeLimit(.minutes(1))) struct FileLimitsTests {
    @Test func loadAnywayStopsAtTheHardLimit() {
        #expect(FileLimits.canForceLoad(size: FileLimits.hardLimit - 1))
        #expect(FileLimits.canForceLoad(size: FileLimits.hardLimit))
        #expect(!FileLimits.canForceLoad(size: FileLimits.hardLimit + 1))
        #expect(!FileLimits.canForceLoad(size: .max))
    }

    @Test func markdownByTheHighlighterExtensionsIsPreviewable() {
        for name in ["README.md", "notes.markdown", "CHANGELOG.MD", "docs/a.b.Markdown"] {
            #expect(MarkdownDocument.isPreviewable(name: name, byteCount: 10), "\(name)")
            #expect(Highlighter.language(forFileName: name) == "markdown", "\(name)")
        }
        for name in ["main.rs", "markdown", "README", "README.md.txt", "notes.mdx", ""] {
            #expect(!MarkdownDocument.isPreviewable(name: name, byteCount: 10), "\(name)")
        }
    }

    @Test func thePreviewStopsAtThePreviewLimit() {
        let limit = MarkdownDocument.previewLimit
        #expect(MarkdownDocument.isPreviewable(name: "README.md", byteCount: 0))
        #expect(MarkdownDocument.isPreviewable(name: "README.md", byteCount: limit - 1))
        #expect(MarkdownDocument.isPreviewable(name: "README.md", byteCount: limit))
        #expect(!MarkdownDocument.isPreviewable(name: "README.md", byteCount: limit + 1))
    }

    @Test func aLargePreviewShowsABuildingPlaceholderAtOnce() {
        let threshold = MarkdownDocument.buildingPlaceholderThreshold
        #expect(MarkdownDocument.buildingPlaceholder(name: "README.md", byteCount: 0) == nil)
        #expect(
            MarkdownDocument.buildingPlaceholder(name: "README.md", byteCount: threshold) == nil)
        #expect(
            MarkdownDocument.buildingPlaceholder(name: "README.md", byteCount: threshold + 1)
                == "Building the preview of README.md…")
        #expect(
            MarkdownDocument.buildingPlaceholder(
                name: "README.md", byteCount: MarkdownDocument.previewLimit) != nil)
    }
}
