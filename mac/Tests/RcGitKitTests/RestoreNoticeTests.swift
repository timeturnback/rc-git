import Testing

@testable import RcGitKit

/// The banner shown when saved tabs did not reopen at launch.
@Suite(.timeLimit(.minutes(1))) struct RestoreNoticeTests {
    @Test func everyTabOpenedShowsNoNotice() {
        #expect(RestoreNotice.make(failed: [], kept: ["/r/a"]) == nil)
    }

    @Test func oneDroppedTabIsNamedAsRemoved() throws {
        let notice = try #require(RestoreNotice.make(failed: ["/gone/repo"], kept: ["/r/a"]))
        #expect(notice.summary == "Could not reopen 1 saved tab: repo (removed from saved tabs)")
        #expect(notice.detail == "/gone/repo: removed from saved tabs")
    }

    @Test func aKeptTabSaysItIsRetriedAndEachFailureIsListed() throws {
        let notice = try #require(
            RestoreNotice.make(
                failed: ["/Volumes/Ext/x", "/gone/y"], kept: ["/r/a", "/Volumes/Ext/x"]))
        #expect(
            notice.summary
                == "Could not reopen 2 saved tabs: x (not reachable, will retry next launch), "
                + "y (removed from saved tabs)")
        #expect(
            notice.detail
                == "/Volumes/Ext/x: not reachable, will retry next launch\n"
                + "/gone/y: removed from saved tabs")
    }

    @Test func aRootPathIsNamedByTheWholePath() throws {
        let notice = try #require(RestoreNotice.make(failed: ["/"], kept: []))
        #expect(notice.summary == "Could not reopen 1 saved tab: / (removed from saved tabs)")
    }
}
