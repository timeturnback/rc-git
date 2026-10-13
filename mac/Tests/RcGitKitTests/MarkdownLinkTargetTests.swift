import Foundation
import Testing

@testable import RcGitKit

@Suite struct MarkdownLinkTargetTests {
    private func resolve(_ destination: String, from source: String? = "docs/guide/index.md")
        -> MarkdownLinkTarget?
    {
        MarkdownLinkTarget.resolve(destination, from: source)
    }

    @Test func webSchemesGoToTheBrowser() {
        #expect(resolve("https://example.com") == .web(URL(string: "https://example.com")!))
        #expect(resolve("http://example.com") == .web(URL(string: "http://example.com")!))
        #expect(resolve("HTTPS://example.com") == .web(URL(string: "HTTPS://example.com")!))
        #expect(resolve("mailto:a@b.com") == .web(URL(string: "mailto:a@b.com")!))
        #expect(resolve("//example.com/x") == .web(URL(string: "https://example.com/x")!))
    }

    @Test func otherSchemesAreNotWebLinks() {
        #expect(resolve("smb://h/share") == .otherScheme(URL(string: "smb://h/share")!))
        #expect(resolve("ssh://h") == .otherScheme(URL(string: "ssh://h")!))
        #expect(resolve("vnc://h") == .otherScheme(URL(string: "vnc://h")!))
        #expect(resolve("x-man-page://ls") == .otherScheme(URL(string: "x-man-page://ls")!))
        #expect(resolve("vscode://x") == .otherScheme(URL(string: "vscode://x")!))
        #expect(
            resolve("applescript://com.apple.scripteditor?action=new")
                == .otherScheme(URL(string: "applescript://com.apple.scripteditor?action=new")!))
        #expect(resolve("my-app+v2:open") == .otherScheme(URL(string: "my-app+v2:open")!))
    }

    @Test func aLocalFileURLIsRefused() {
        #expect(resolve("file:///etc/passwd") == nil)
        #expect(resolve("FILE:///etc/passwd") == nil)
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
        #expect(
            resolve("other.md?raw=1#errors")
                == .repositoryPath("docs/guide/other.md", fragment: "errors"))
        #expect(
            resolve("other.md?plain=1#errors")
                == .repositoryPath("docs/guide/other.md", fragment: "errors"))
    }

    @Test func aQueryOnlyDestinationIsNoFile() {
        #expect(resolve("?") == nil)
        #expect(resolve("??") == nil)
        #expect(resolve(" ? ") == nil)
        #expect(resolve("?a=b") == nil)
        #expect(resolve("?raw=1") == nil)
        #expect(resolve("?tab=1") == nil)
        #expect(resolve("?x#sec") == .fragment("sec"))
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
