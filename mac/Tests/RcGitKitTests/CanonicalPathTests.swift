import Foundation
import Testing

@testable import RcGitKit

/// Every spelling of one repository gives one `tabKey`, the spelling FSEvents reports.
@Suite struct CanonicalPathTests {
    /// `path` under the other name of its `/private` root: `/var/x` for `/private/var/x` and
    /// the reverse. `/tmp` and `/var` are both symlinks into `/private` on macOS.
    private static func otherPrivateSpelling(_ path: String) -> String {
        path.hasPrefix("/private/") ? String(path.dropFirst("/private".count)) : "/private" + path
    }

    @Test func everySpellingOfARepositoryGivesOneKey() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let aliases = try GitFixture.emptyDirectory()
        defer { aliases.remove() }
        let parent = (fixture.path as NSString).deletingLastPathComponent
        let name = (fixture.path as NSString).lastPathComponent
        try FileManager.default.createSymbolicLink(
            atPath: aliases.path + "/parent", withDestinationPath: parent)

        let resolved = try #require(realpath(fixture.path + "/.git", nil))
        defer { free(resolved) }
        let expected = String(cString: resolved)
        #expect(expected.hasPrefix("/private/"))
        let spellings = [
            fixture.path,
            fixture.path + "/.git",
            fixture.path + "/",
            fixture.path + "/.git/",
            fixture.path + "/src/..",
            aliases.path + "/parent/" + name,
            Self.otherPrivateSpelling(fixture.path),
            Self.otherPrivateSpelling(fixture.path) + "/.git",
        ]
        for spelling in spellings {
            let key = try Repository(path: spelling).tabKey
            #expect(key == expected, "\(spelling)")
        }
    }

    @Test func twoRepositoriesGetTwoKeys() throws {
        let first = try GitFixture.standard()
        defer { first.remove() }
        let second = try GitFixture.standard()
        defer { second.remove() }
        #expect(try Repository(path: first.path).tabKey != Repository(path: second.path).tabKey)
    }

    @Test func aLinkedWorktreeHasItsOwnKey() throws {
        let fixture = try GitFixture.standard()
        defer { fixture.remove() }
        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let linkedPath = outside.path + "/linked"
        try fixture.git(["worktree", "add", "-q", "-b", "wt", linkedPath])
        #expect(try Repository(path: fixture.path).tabKey != Repository(path: linkedPath).tabKey)
    }

    @Test func resolveKeepsPrivateAndMakesARelativePathAbsolute() {
        #expect(CanonicalPath.resolve("/var") == "/private/var")
        #expect(CanonicalPath.resolve("/tmp/../var/") == "/private/var")
        let missing = "/private/var/rc-git-no-such-\(UUID().uuidString)"
        #expect(CanonicalPath.resolve(missing) == missing)
        #expect(CanonicalPath.resolve(".").hasPrefix("/"))
    }
}
