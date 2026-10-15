import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct RepoPathTests {
    @Test func climbingPastTheRootIsNil() {
        #expect(RepoPath.flatten([], "..") == nil)
        #expect(RepoPath.flatten(["a"], "../..") == nil)
        #expect(RepoPath.flatten(["a"], "../../b") == nil)
    }

    @Test func climbingToTheRootIsEmpty() {
        #expect(RepoPath.flatten(["a", "b"], "../..") == [])
    }

    @Test func aDotAloneIsTheBase() {
        #expect(RepoPath.flatten(["a", "b"], ".") == ["a", "b"])
    }

    @Test func anEmptyRelativeIsTheBase() {
        #expect(RepoPath.flatten(["a", "b"], "") == ["a", "b"])
        #expect(RepoPath.flatten([], "") == [])
    }

    @Test func repeatedSlashesCollapse() {
        #expect(RepoPath.flatten(["a"], "b//c///d") == ["a", "b", "c", "d"])
        #expect(RepoPath.flatten(["a"], ".//..//b/") == ["b"])
    }

    @Test func dotsMixWithNames() {
        #expect(RepoPath.flatten(["src", "lib"], "./../util/./x.ts") == ["src", "util", "x.ts"])
    }
}
