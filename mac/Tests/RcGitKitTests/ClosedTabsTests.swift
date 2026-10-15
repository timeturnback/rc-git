import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct ClosedTabsTests {
    @Test func emptyHasNothingToReopen() {
        var closed = ClosedTabs()
        #expect(!closed.canReopen(skipping: []))
        #expect(closed.pop(skipping: []) == nil)
    }

    @Test func popsMostRecentFirst() {
        var closed = ClosedTabs()
        closed.push("/a")
        closed.push("/b")
        #expect(closed.pop(skipping: []) == "/b")
        #expect(closed.pop(skipping: []) == "/a")
        #expect(closed.pop(skipping: []) == nil)
    }

    @Test func closingAgainMovesToTop() {
        var closed = ClosedTabs()
        closed.push("/a")
        closed.push("/b")
        closed.push("/a")
        #expect(closed.paths == ["/b", "/a"])
    }

    @Test func skipsAndDropsRepositoriesAlreadyOpen() {
        var closed = ClosedTabs()
        closed.push("/a")
        closed.push("/b")
        closed.push("/c")
        #expect(closed.canReopen(skipping: ["/c"]))
        #expect(closed.pop(skipping: ["/c", "/b"]) == "/a")
        #expect(closed.paths.isEmpty)
    }

    @Test func nothingToReopenWhenEveryEntryIsOpen() {
        var closed = ClosedTabs()
        closed.push("/a")
        #expect(!closed.canReopen(skipping: ["/a"]))
        #expect(closed.pop(skipping: ["/a"]) == nil)
    }

    @Test func capsAtCapacityDroppingOldest() {
        var closed = ClosedTabs()
        for index in 0...ClosedTabs.capacity { closed.push("/repo\(index)") }
        #expect(closed.paths.count == ClosedTabs.capacity)
        #expect(!closed.paths.contains("/repo0"))
        #expect(closed.paths.last == "/repo\(ClosedTabs.capacity)")
    }
}
