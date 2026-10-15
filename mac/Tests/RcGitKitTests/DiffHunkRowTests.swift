import Testing

@testable import RcGitKit

/// Which hunk a row of the Changes diff pane belongs to: a header row, then one row per line.
@Suite(.timeLimit(.minutes(1))) struct DiffHunkRowTests {
    static let first = DiffHunk(
        oldStart: 1, oldLines: 1, newStart: 1, newLines: 2,
        lines: [DiffLine(kind: .context, text: "a"), DiffLine(kind: .added, text: "b")])
    static let second = DiffHunk(
        oldStart: 9, oldLines: 1, newStart: 10, newLines: 0,
        lines: [DiffLine(kind: .removed, text: "z")])
    static let hunks = [first, second]

    @Test func aHeaderRowOwnsItsHunk() {
        #expect(DiffHunk.owning(row: 0, in: Self.hunks) == Self.first)
        #expect(DiffHunk.owning(row: 3, in: Self.hunks) == Self.second)
    }

    @Test func aLineRowBelongsToTheHunkAboveIt() {
        #expect(DiffHunk.owning(row: 1, in: Self.hunks) == Self.first)
        #expect(DiffHunk.owning(row: 2, in: Self.hunks) == Self.first)
        #expect(DiffHunk.owning(row: 4, in: Self.hunks) == Self.second)
    }

    @Test func aRowOutsideThePaneHasNoHunk() {
        #expect(DiffHunk.owning(row: 5, in: Self.hunks) == nil)
        #expect(DiffHunk.owning(row: -1, in: Self.hunks) == nil)
        #expect(DiffHunk.owning(row: 0, in: []) == nil)
    }
}
