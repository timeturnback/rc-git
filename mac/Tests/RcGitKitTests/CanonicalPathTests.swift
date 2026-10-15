import Foundation
import Testing

@testable import RcGitKit

/// Every spelling of one repository gives one `tabKey`, the spelling FSEvents reports.
@Suite(.timeLimit(.minutes(1))) struct CanonicalPathTests {
    /// `path` under the other name of its `/private` root: `/var/x` for `/private/var/x` and
    /// the reverse. `/tmp` and `/var` are both symlinks into `/private` on macOS.
    private static func otherPrivateSpelling(_ path: String) -> String {
        path.hasPrefix("/private/") ? String(path.dropFirst("/private".count)) : "/private" + path
    }

    @Test func everySpellingOfARepositoryGivesOneKey() async throws {
        let fixture = try await GitFixture.standard()
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

    @Test func twoRepositoriesGetTwoKeys() async throws {
        let first = try await GitFixture.standard()
        defer { first.remove() }
        let second = try await GitFixture.standard()
        defer { second.remove() }
        #expect(try Repository(path: first.path).tabKey != Repository(path: second.path).tabKey)
    }

    @Test func aLinkedWorktreeHasItsOwnKey() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let linkedPath = outside.path + "/linked"
        try await fixture.git(["worktree", "add", "-q", "-b", "wt", linkedPath])
        #expect(try Repository(path: fixture.path).tabKey != Repository(path: linkedPath).tabKey)
    }

    @Test func aRepositoryOpenedThroughItsGitDirShowsItsWorkingDirectory() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let workDir = CanonicalPath.resolve(fixture.path)
        #expect(workDir.hasPrefix("/private/"))
        for spelling in [fixture.path, fixture.path + "/.git", fixture.path + "/.git/"] {
            let repository = try Repository(path: spelling)
            #expect(repository.tabPath(openedAs: spelling) == workDir, "\(spelling)")
        }
    }

    @Test func aBareRepositoryShowsThePathItWasOpenedThrough() async throws {
        let fixture = try await GitFixture.standard()
        defer { fixture.remove() }
        let outside = try GitFixture.emptyDirectory()
        defer { outside.remove() }
        let barePath = outside.path + "/bare.git"
        try await fixture.git(["clone", "-q", "--bare", fixture.path, barePath])
        let repository = try Repository(path: barePath)
        #expect(repository.tabPath(openedAs: barePath) == CanonicalPath.resolve(barePath))
    }

    @Test func resolveKeepsPrivateAndMakesARelativePathAbsolute() {
        #expect(CanonicalPath.resolve("/var") == "/private/var")
        #expect(CanonicalPath.resolve("/tmp/../var/") == "/private/var")
        let missing = "/private/var/rc-git-no-such-\(UUID().uuidString)"
        #expect(CanonicalPath.resolve(missing) == missing)
        #expect(CanonicalPath.resolve(".").hasPrefix("/"))
    }
}
