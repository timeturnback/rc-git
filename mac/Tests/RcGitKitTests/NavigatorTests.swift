import Foundation
import Testing

@testable import RcGitKit

@Suite struct NavigatorTests {
    private final class Tab {}

    private let one = Tab()
    private let two = Tab()

    private func stop(_ repository: String = "/one", file: String? = nil) -> NavigationStop {
        NavigationStop(
            repositoryPath: repository, mode: .files,
            browser: BrowserLocation(tree: .workingTree, filePath: file))
    }

    private func id(_ tab: Tab) -> ObjectIdentifier {
        ObjectIdentifier(tab)
    }

    @Test func aBackgroundTabsReportIsIgnored() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))

        let recorded = navigator.report(stop("/two", file: "b"), from: id(two))

        #expect(!recorded)
        #expect(navigator.list.stops == [stop(file: "a")])
    }

    @Test func aTabWithNoStopYetRecordsOnceInFront() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: nil)

        let recorded = navigator.report(stop(file: "a"), from: id(one))
        #expect(recorded)
        #expect(navigator.list.stops == [stop(file: "a")])
    }

    @Test func aReportDuringARestoreIsIgnoredAndForwardSurvives() throws {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))
        navigator.report(stop(file: "b"), from: id(one))

        let stepped = navigator.go(.back)
        let back = try #require(stepped)
        navigator.beginRestore(in: id(one))
        let recorded = navigator.report(stop(file: "halfway"), from: id(one))
        navigator.report(back, from: id(one))
        navigator.endRestore()

        #expect(!recorded)
        #expect(back == stop(file: "a"))
        #expect(navigator.list.stops == [stop(file: "a"), stop(file: "b")])
        #expect(navigator.canGo(.forward))
        let forward = navigator.go(.forward)
        #expect(forward == stop(file: "b"))
    }

    @Test func aReportAfterTheRestoreEndsIsRecorded() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))
        navigator.report(stop(file: "b"), from: id(one))
        let back = navigator.go(.back)
        #expect(back != nil)
        navigator.beginRestore(in: id(one))
        navigator.endRestore()

        let recorded = navigator.report(stop(file: "c"), from: id(one))
        #expect(recorded)
        #expect(navigator.list.stops == [stop(file: "a"), stop(file: "c")])
    }

    @Test func aRestoreBringsItsTabToTheFront() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))
        navigator.tabCameToFront(id(two), at: stop("/two", file: "b"))
        let back = navigator.go(.back)
        #expect(back != nil)
        navigator.beginRestore(in: id(one))
        navigator.endRestore()

        let frontRecorded = navigator.report(stop(file: "c"), from: id(one))
        #expect(frontRecorded)
        let backgroundRecorded = navigator.report(stop("/two", file: "d"), from: id(two))
        #expect(!backgroundRecorded)
    }

    @Test func switchingTabsRecordsTheNewTabsStop() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))

        let recorded = navigator.tabCameToFront(id(two), at: stop("/two", file: "b"))

        #expect(recorded)
        #expect(navigator.list.stops == [stop(file: "a"), stop("/two", file: "b")])
        let frontRecorded = navigator.report(stop("/two", file: "c"), from: id(two))
        #expect(frontRecorded)
        let backgroundRecorded = navigator.report(stop(file: "d"), from: id(one))
        #expect(!backgroundRecorded)
    }

    @Test func theSameTabBecomingMainAgainRecordsNothing() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))
        navigator.report(stop(file: "b"), from: id(one))
        let back = navigator.go(.back)
        #expect(back != nil)

        let recorded = navigator.tabCameToFront(id(one), at: stop(file: "b"))

        #expect(!recorded)
        #expect(navigator.list.current == stop(file: "a"))
        #expect(navigator.canGo(.forward))
    }

    @Test func closingATabForgetsItsStops() {
        var navigator = Navigator()
        navigator.tabCameToFront(id(one), at: stop(file: "a"))
        navigator.tabCameToFront(id(two), at: stop("/two", file: "b"))

        navigator.tabClosed(id(two), repositoryPath: "/two")

        #expect(navigator.list.stops == [stop(file: "a")])
        let closedRecorded = navigator.report(stop("/two", file: "c"), from: id(two))
        #expect(!closedRecorded)
    }
}
