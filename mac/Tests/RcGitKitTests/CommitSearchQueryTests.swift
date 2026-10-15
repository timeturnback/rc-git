import Testing

@testable import RcGitKit

/// Which submitted History searches try a revision jump.
@Suite(.timeLimit(.minutes(1))) struct CommitSearchQueryTests {
    @Test func fourToFortyASCIIHexDigitsAreARevision() {
        #expect(CommitSearchQuery.revision("abc1", scope: .message) == "abc1")
        #expect(CommitSearchQuery.revision("2024", scope: .message) == "2024")
        #expect(CommitSearchQuery.revision("DEADbeef", scope: .message) == "DEADbeef")
        let full = String(repeating: "f", count: 40)
        #expect(CommitSearchQuery.revision(full, scope: .message) == full)
    }

    @Test func lengthsOutsideTheRangeAreSearchText() {
        #expect(CommitSearchQuery.revision("abc", scope: .message) == nil)
        #expect(
            CommitSearchQuery.revision(String(repeating: "f", count: 41), scope: .message) == nil)
        #expect(CommitSearchQuery.revision("", scope: .message) == nil)
    }

    @Test func surroundingWhitespaceIsTrimmed() {
        #expect(CommitSearchQuery.revision("  cafe \t", scope: .message) == "cafe")
        #expect(CommitSearchQuery.revision(" abc ", scope: .message) == nil)
    }

    @Test func nonASCIIHexIsSearchText() {
        #expect(
            CommitSearchQuery.revision("\u{FF11}\u{FF12}\u{FF13}\u{FF14}", scope: .message) == nil)
        #expect(CommitSearchQuery.revision("cafe\u{301}", scope: .message) == nil)
        #expect(CommitSearchQuery.revision("feedg", scope: .message) == nil)
        #expect(CommitSearchQuery.revision("ab cd", scope: .message) == nil)
    }

    @Test func theAuthorScopeNeverJumps() {
        #expect(CommitSearchQuery.revision("abc1", scope: .author) == nil)
        #expect(CommitSearchQuery.revision("2024", scope: .author) == nil)
    }
}
