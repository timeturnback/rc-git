import Foundation
import Testing

@testable import RcGitKit

@Suite struct MarkdownLinkTargetTests {
    private func resolve(_ destination: String, from source: String? = "docs/guide/index.md")
        -> MarkdownLinkTarget?
    {
        MarkdownLinkTarget.resolve(destination, from: source)
    }

    @Test func schemesGoToTheBrowser() {
        #expect(resolve("https://example.com") == .external(URL(string: "https://example.com")!))
        #expect(resolve("mailto:a@b.com") == .external(URL(string: "mailto:a@b.com")!))
        #expect(resolve("//example.com/x") == .external(URL(string: "https://example.com/x")!))
    }

    @Test func aLocalFileURLIsRefused() {
        #expect(resolve("file:///etc/passwd") == nil)
    }

    @Test func aBareFragmentStaysInTheOpenFile() {
        #expect(resolve("#invariants") == .fragment("invariants"))
    }

    @Test func relativePathsResolveAgainstTheOpenFilesDirectory() {
        #expect(resolve("other.md") == .repositoryPath("docs/guide/other.md", fragment: nil))
        #expect(resolve("./other.md") == .repositoryPath("docs/guide/other.md", fragment: nil))
        #expect(resolve("../ffi.md") == .repositoryPath("docs/ffi.md", fragment: nil))
        #expect(resolve("../../README.md") == .repositoryPath("README.md", fragment: nil))
    }

    @Test func aLeadingSlashMeansTheRepositoryRoot() {
        #expect(resolve("/CONCEPTS.md") == .repositoryPath("CONCEPTS.md", fragment: nil))
    }

    @Test func withoutAnOpenPathLinksResolveFromTheRoot() {
        #expect(resolve("docs/x.md", from: nil) == .repositoryPath("docs/x.md", fragment: nil))
    }

    @Test func fragmentsAndQueriesAreSplitOff() {
        #expect(resolve("../ffi.md#errors") == .repositoryPath("docs/ffi.md", fragment: "errors"))
        #expect(resolve("other.md?raw=1") == .repositoryPath("docs/guide/other.md", fragment: nil))
    }

    @Test func percentEscapesAreDecoded() {
        #expect(
            resolve("../my%20file.md") == .repositoryPath("docs/my file.md", fragment: nil))
    }

    @Test func climbingAboveTheRepositoryIsRefused() {
        #expect(resolve("../../../outside.md") == nil)
        #expect(resolve("", from: "README.md") == nil)
    }
}
