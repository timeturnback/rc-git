import Foundation
import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct RecentRepositoriesTests {
    @Test func insertingMovesToFrontAndDeduplicates() {
        let list = RecentRepositories.inserting("/b", into: ["/a", "/b", "/c"])
        #expect(list == ["/b", "/a", "/c"])
    }

    @Test func insertingCapsTheList() {
        let existing = (0..<RecentRepositories.capacity).map { "/repo\($0)" }
        let list = RecentRepositories.inserting("/new", into: existing)
        #expect(list.count == RecentRepositories.capacity)
        #expect(list.first == "/new")
        #expect(!list.contains("/repo\(RecentRepositories.capacity - 1)"))
    }

    @Test func aBlankQueryListsEveryRecentInRecencyOrder() {
        let paths = ["/work/b", "/work/a", "/work/c"]
        #expect(RecentRepositories.matching(paths, query: "") == paths)
        #expect(RecentRepositories.matching(paths, query: "  ") == paths)
        #expect(RecentRepositories.matching([], query: "a").isEmpty)
    }

    @Test func aQueryKeepsOnlyMatchesAndRanksTheFolderNameFirst() {
        let paths = ["/src/notes", "/src/rc-git", "/rc/git-tools/web"]
        #expect(RecentRepositories.matching(paths, query: "notes") == ["/src/notes"])
        #expect(
            RecentRepositories.matching(paths, query: "rcgit")
                == ["/src/rc-git", "/rc/git-tools/web"])
        #expect(RecentRepositories.matching(paths, query: "zzz").isEmpty)
        #expect(
            RecentRepositories.matching(paths, query: "git").first == "/src/rc-git",
            "a match inside the folder name beats one in a parent directory")
    }

    @Test func equalScoresKeepRecencyOrder() {
        let paths = ["/a/app", "/b/app", "/c/app"]
        #expect(RecentRepositories.matching(paths, query: "app") == paths)
        #expect(RecentRepositories.matching(paths, query: " APP ") == paths)
    }

    @Test func aDecomposedQueryMatchesAPrecomposedPath() {
        let precomposed = "/src/caf\u{E9}"
        let decomposed = "cafe\u{301}"
        #expect(RecentRepositories.matching([precomposed], query: decomposed) == [precomposed])
    }

    @Test func aQueryTypedAsDisplayedMatchesUnderTheHomeDirectory() {
        let home = NSHomeDirectory()
        let inside = home + "/code/app"
        let outside = "/opt/code/app"
        #expect(RecentRepositories.displayPath(inside) == "~/code/app")
        #expect(RecentRepositories.displayPath(outside) == outside)
        #expect(RecentRepositories.matching([outside, inside], query: "~/co") == [inside])
    }

    @Test func theEmptyNoteSaysWhetherAnyRecentExists() {
        #expect(RecentRepositories.emptyNote(hasRecents: false) == "No recent repositories")
        #expect(RecentRepositories.emptyNote(hasRecents: true) == "No recent repository matches")
    }

    @Test func persistsThroughDefaults() throws {
        try withScratchDefaults { defaults in
            let recents = RecentRepositories(defaults: defaults)
            #expect(recents.paths.isEmpty)
            recents.note("/one")
            recents.note("/two")
            #expect(recents.paths == ["/two", "/one"])
            recents.clear()
            #expect(recents.paths.isEmpty)
        }
    }

    /// Another build under the same bundle id may have written this key with another type;
    /// that reads as no recents, and the next open replaces it.
    @Test func aWronglyTypedStoredListReadsAsEmpty() throws {
        try withScratchDefaults { defaults in
            let recents = RecentRepositories(defaults: defaults)
            defaults.set("oops", forKey: RecentRepositories.key)
            #expect(recents.paths.isEmpty)

            defaults.set(["/a", 3] as [Any], forKey: RecentRepositories.key)
            #expect(recents.paths.isEmpty)

            recents.note("/b")
            #expect(recents.paths == ["/b"])
        }
    }
}
