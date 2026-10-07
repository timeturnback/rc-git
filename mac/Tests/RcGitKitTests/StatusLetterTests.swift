import Testing

@testable import RcGitKit

/// The one-letter labels the changed-file rows and tree badges show.
@Suite struct StatusLetterTests {
    @Test func worktreeStatusLettersMatchGitStatus() {
        #expect(WorktreeStatus.modified.letter == "M")
        #expect(WorktreeStatus.untracked.letter == "U")
        #expect(WorktreeStatus.ignored.letter == "I")
        #expect(WorktreeStatus.deleted.letter == "D")
    }

    @Test func anUnchangedPathHasNoLetter() {
        #expect(WorktreeStatus.unchanged.letter == nil)
    }

    @Test func changeKindLettersMatchGitStatus() {
        #expect(ChangeKind.added.letter == "A")
        #expect(ChangeKind.deleted.letter == "D")
        #expect(ChangeKind.modified.letter == "M")
        #expect(ChangeKind.renamed.letter == "R")
        #expect(ChangeKind.copied.letter == "C")
    }
}
