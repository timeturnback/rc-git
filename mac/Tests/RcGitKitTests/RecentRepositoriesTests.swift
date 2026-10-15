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
