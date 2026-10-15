import Foundation
import Testing

@testable import RcGitKit

@Suite(.timeLimit(.minutes(1))) struct BackForwardListTests {
    private func stop(
        _ repository: String = "/one", mode: ContentMode = .files, file: String? = nil,
        commit: Oid? = nil
    ) -> NavigationStop {
        NavigationStop(
            repositoryPath: repository, mode: mode,
            browser: BrowserLocation(tree: .workingTree, filePath: file, detailCommit: commit))
    }

    private func list(_ stops: NavigationStop...) -> BackForwardList {
        var list = BackForwardList()
        for stop in stops { list.visit(stop) }
        return list
    }

    @Test func anEmptyListGoesNowhere() {
        var list = BackForwardList()
        #expect(list.current == nil)
        #expect(!list.canGo(.back))
        #expect(!list.canGo(.forward))
        #expect(list.go(.back) == nil)
        #expect(list.go(.forward) == nil)
    }

    @Test func aVisitBecomesTheCurrentStop() {
        let list = list(stop(file: "a"), stop(file: "b"))
        #expect(list.stops.count == 2)
        #expect(list.current == stop(file: "b"))
        #expect(list.canGo(.back))
        #expect(!list.canGo(.forward))
    }

    @Test func visitingTheCurrentStopRecordsNothing() {
        var list = list(stop(file: "a"), stop(file: "b"))
        list.visit(stop(file: "b"))
        #expect(list.stops.count == 2)
        _ = list.go(.back)
        list.visit(stop(file: "a"))
        #expect(list.stops.count == 2, "a no-op visit keeps the stops ahead")
        #expect(list.canGo(.forward))
    }

    @Test func steppingMovesTheMarkAndStopsAtEitherEnd() {
        var list = list(stop(file: "a"), stop(file: "b"), stop(file: "c"))
        #expect(list.go(.back) == stop(file: "b"))
        #expect(list.go(.back) == stop(file: "a"))
        #expect(list.go(.back) == nil)
        #expect(list.current == stop(file: "a"))
        #expect(list.go(.forward) == stop(file: "b"))
        #expect(list.go(.forward) == stop(file: "c"))
        #expect(list.go(.forward) == nil)
        #expect(list.stops.count == 3, "stepping never drops a stop")
    }

    @Test func aVisitAfterSteppingBackDropsTheStopsAhead() {
        var list = list(stop(file: "a"), stop(file: "b"), stop(file: "c"))
        _ = list.go(.back)
        _ = list.go(.back)
        list.visit(stop(file: "d"))
        #expect(list.stops == [stop(file: "a"), stop(file: "d")])
        #expect(list.current == stop(file: "d"))
        #expect(!list.canGo(.forward))
    }

    @Test func theOldestStopsFallOffPastTheLimit() {
        var list = BackForwardList()
        for index in 0..<(BackForwardList.limit + 5) {
            list.visit(stop(file: "\(index)"))
        }
        #expect(list.stops.count == BackForwardList.limit)
        #expect(list.stops.first == stop(file: "5"))
        #expect(list.current == stop(file: "\(BackForwardList.limit + 4)"))
    }

    @Test func closingATabDropsItsStopsAndKeepsTheMark() {
        var list = list(
            stop("/one", file: "a"), stop("/two", file: "b"), stop("/one", file: "c"),
            stop("/two", file: "d"))
        _ = list.go(.back)
        list.removeStops(inRepository: "/two")
        #expect(list.stops == [stop("/one", file: "a"), stop("/one", file: "c")])
        #expect(list.current == stop("/one", file: "c"))
    }

    @Test func droppingTheCurrentStopFallsBackToTheOneBefore() {
        var list = list(stop("/one", file: "a"), stop("/two", file: "b"), stop("/one", file: "c"))
        _ = list.go(.back)
        list.removeStops(inRepository: "/two")
        #expect(list.current == stop("/one", file: "a"))
        #expect(list.canGo(.forward))
    }

    @Test func droppingTheFirstAndCurrentStopLandsOnTheFirstLeft() {
        var list = list(stop("/two", file: "a"), stop("/one", file: "b"), stop("/one", file: "c"))
        _ = list.go(.back)
        _ = list.go(.back)
        list.removeStops(inRepository: "/two")
        #expect(list.current == stop("/one", file: "b"))
        #expect(!list.canGo(.back))
    }

    @Test func droppingEveryStopEmptiesTheList() {
        var list = list(stop("/two", file: "a"), stop("/two", file: "b"))
        list.removeStops(inRepository: "/two")
        #expect(list.stops.isEmpty)
        #expect(list.current == nil)
        #expect(!list.canGo(.back))
        list.visit(stop("/one", file: "c"))
        #expect(list.current == stop("/one", file: "c"))
    }

    @Test func droppingStopsMergesNeighborsThatBecameTheSamePlace() {
        var list = list(stop("/one", file: "a"), stop("/two", file: "b"), stop("/one", file: "a"))
        list.removeStops(inRepository: "/two")
        #expect(list.stops == [stop("/one", file: "a")])
        #expect(list.current == stop("/one", file: "a"))
        #expect(!list.canGo(.back), "a step back must change what is on screen")
    }

    @Test func theDetailArrowNeedsAnotherCommitBehindItInTheSameHistory() {
        let first = oid(0x01)
        let second = oid(0x02)
        let twoCommits = list(
            stop(mode: .history, commit: first), stop(mode: .history, commit: second))
        #expect(twoCommits.backReturnsToCommit(inRepository: "/one"))
        #expect(!twoCommits.backReturnsToCommit(inRepository: "/two"), "another tab's stops")

        let fromNoCommit = list(stop(mode: .history), stop(mode: .history, commit: first))
        #expect(!fromNoCommit.backReturnsToCommit(inRepository: "/one"))

        let fromFiles = list(
            stop(mode: .files, commit: first), stop(mode: .history, commit: second))
        #expect(!fromFiles.backReturnsToCommit(inRepository: "/one"))

        let fromAnotherTab = list(
            stop("/two", mode: .history, commit: first), stop(mode: .history, commit: second))
        #expect(!fromAnotherTab.backReturnsToCommit(inRepository: "/one"))

        #expect(!BackForwardList().backReturnsToCommit(inRepository: "/one"))
    }

    @Test func mouseButtonsAndSwipesNameADirection() {
        #expect(BackForwardList.Direction(mouseButton: 3) == .back)
        #expect(BackForwardList.Direction(mouseButton: 4) == .forward)
        #expect(BackForwardList.Direction(mouseButton: 2) == nil, "the middle button")
        #expect(BackForwardList.Direction(swipeAmount: 1) == .back)
        #expect(BackForwardList.Direction(swipeAmount: -0.4) == .forward)
        #expect(BackForwardList.Direction(swipeAmount: 0) == nil)
    }
}
