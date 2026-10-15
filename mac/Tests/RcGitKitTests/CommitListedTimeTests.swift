import Testing

@testable import RcGitKit

/// Which time the History Date column shows for a commit.
@Suite(.timeLimit(.minutes(1))) struct CommitListedTimeTests {
    private let rebased = CommitSummary(
        oid: Oid(hex: "1a2b3c4d5e6f708192a3b4c5d6e7f80910a1b2c3")!, parents: [],
        authorName: "A", authorEmail: "a@example.com", authorTime: 1_704_412_800,
        authorOffset: 0, committerTime: 1_704_585_600, title: "rebased")

    @Test func noFilterShowsAuthorTime() {
        #expect(rebased.listedTime(filter: nil) == 1_704_412_800)
    }

    @Test func aFilterWithoutDatesShowsAuthorTime() {
        let filter = CommitFilter(message: "fix", author: "a", path: "src")
        #expect(rebased.listedTime(filter: filter) == 1_704_412_800)
    }

    @Test func aSinceFilterShowsCommitterTime() {
        let filter = CommitFilter(after: 1_704_499_200)
        #expect(rebased.listedTime(filter: filter) == 1_704_585_600)
    }

    @Test func anUntilFilterShowsCommitterTime() {
        let filter = CommitFilter(message: "fix", before: 1_704_672_000)
        #expect(rebased.listedTime(filter: filter) == 1_704_585_600)
    }
}
