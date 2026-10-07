import Testing

@testable import RcGitKit

@Suite struct BranchBannerTests {
    private let tip = Oid(hex: "1a2b3c4d5e6f708192a3b4c5d6e7f80910a1b2c3")!
    private let other = Oid(hex: "ffeeddccbbaa99887766554433221100ffeeddcc")!

    private func commit(_ title: String) -> CommitSummary {
        CommitSummary(
            oid: other, parents: [], authorName: "A", authorEmail: "a@example.com",
            authorTime: 0, authorOffset: 0, title: title)
    }

    // MARK: Files

    @Test func theWorkingTreeNamesTheBranchUnderIt() {
        let banner = BranchBanner.browsing(
            selectedRef: nil, browsedCommit: nil, browsingWorkingTree: true,
            head: Head(refName: "refs/heads/feat/banner", target: tip))
        #expect(
            banner
                == BranchBanner(
                    symbol: .workingTree, title: "Working Tree", detail: "on feat/banner"))
    }

    @Test func aDetachedWorkingTreeNamesItsCommit() {
        let banner = BranchBanner.browsing(
            selectedRef: nil, browsedCommit: nil, browsingWorkingTree: true,
            head: Head(refName: nil, target: tip))
        #expect(banner.detail == "detached at 1a2b3c4")
    }

    @Test func theWorkingTreeWinsOverAStaleSelection() {
        let banner = BranchBanner.browsing(
            selectedRef: branch("main", at: tip), browsedCommit: commit("Old"),
            browsingWorkingTree: true,
            head: nil)
        #expect(banner.symbol == .workingTree)
        #expect(banner.detail == nil)
    }

    @Test func aBrowsedCommitNamesItsIdAndTitle() {
        let banner = BranchBanner.browsing(
            selectedRef: nil, browsedCommit: commit("Fix the parser"), browsingWorkingTree: false,
            head: nil)
        #expect(banner == BranchBanner(symbol: .commit, title: "ffeeddc", detail: "Fix the parser"))
    }

    @Test func aSelectedRefNamesItselfWithItsKind() {
        let tag = RefEntry(fullName: "refs/tags/v1.0", shortName: "v1.0", kind: .tag, target: tip)
        let banner = BranchBanner.browsing(
            selectedRef: tag, browsedCommit: nil, browsingWorkingTree: false, head: nil)
        #expect(banner == BranchBanner(symbol: .ref(.tag), title: "v1.0"))
    }

    @Test func nothingBrowsedIsTheNothingBanner() {
        let banner = BranchBanner.browsing(
            selectedRef: nil, browsedCommit: nil, browsingWorkingTree: false, head: nil)
        #expect(banner == .nothing)
    }

    // MARK: Changes

    @Test func changesNameTheCheckedOutBranch() {
        let banner = BranchBanner.checkedOut(head: Head(refName: "refs/heads/main", target: tip))
        #expect(banner == BranchBanner(symbol: .ref(.localBranch), title: "main"))
    }

    @Test func anUnbornBranchSaysItHasNoCommits() {
        let banner = BranchBanner.checkedOut(head: Head(refName: "refs/heads/main", target: nil))
        #expect(banner.title == "main")
        #expect(banner.detail == "No commits yet")
    }

    @Test func aDetachedHeadNamesItsCommit() {
        let banner = BranchBanner.checkedOut(head: Head(refName: nil, target: tip))
        #expect(
            banner == BranchBanner(symbol: .detached, title: "Detached HEAD", detail: "1a2b3c4"))
    }

    @Test func noHeadIsTheNothingBanner() {
        #expect(BranchBanner.checkedOut(head: nil) == .nothing)
        #expect(BranchBanner.checkedOut(head: Head(refName: nil, target: nil)) == .nothing)
    }

    @Test func aRefNameOutsideTheBranchNamespaceStaysWhole() {
        let banner = BranchBanner.checkedOut(head: Head(refName: "refs/other/x", target: tip))
        #expect(banner.title == "refs/other/x")
    }

    // MARK: Summary

    @Test func theSummaryJoinsTitleAndDetail() {
        #expect(
            BranchBanner(symbol: .commit, title: "ffeeddc", detail: "Fix").summary
                == "ffeeddc — Fix")
        #expect(BranchBanner(symbol: .ref(.localBranch), title: "main").summary == "main")
        #expect(BranchBanner.nothing.summary == "")
    }
}
