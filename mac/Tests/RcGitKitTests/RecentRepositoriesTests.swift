import Foundation
import Testing

@testable import RcGitKit

@Suite struct RecentRepositoriesTests {
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

    @Test func persistsThroughDefaults() {
        let suite = "rc-git.tests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let recents = RecentRepositories(defaults: defaults)
        #expect(recents.paths.isEmpty)
        recents.note("/one")
        recents.note("/two")
        #expect(recents.paths == ["/two", "/one"])
        recents.clear()
        #expect(recents.paths.isEmpty)
    }
}
