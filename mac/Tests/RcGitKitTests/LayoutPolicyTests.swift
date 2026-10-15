import CoreGraphics
import Foundation
import Testing

@testable import RcGitKit

@Suite struct LayoutPolicyTests {
    @Test func wideWindowShowsEverything() {
        let layout = LayoutPolicy.layout(for: CGSize(width: 1400, height: 900))
        #expect(layout == Layout(showsSidebar: true))
    }

    @Test func narrowWindowHidesSidebar() {
        let layout = LayoutPolicy.layout(for: CGSize(width: 500, height: 600))
        #expect(layout == Layout(showsSidebar: false))
    }

    @Test func portraitWindowKeepsTheSidebarRuleOnly() {
        #expect(LayoutPolicy.layout(for: CGSize(width: 900, height: 1400)).showsSidebar)
        #expect(!LayoutPolicy.layout(for: CGSize(width: 480, height: 1400)).showsSidebar)
    }

    @Test func sidebarThresholdIsInclusive() {
        let atSidebar = LayoutPolicy.layout(
            for: CGSize(width: LayoutPolicy.sidebarWindowWidth, height: 400))
        #expect(atSidebar.showsSidebar)
        let belowSidebar = LayoutPolicy.layout(
            for: CGSize(width: LayoutPolicy.sidebarWindowWidth - 1, height: 400))
        #expect(!belowSidebar.showsSidebar)
    }

    @Test func sidebarWidthFollowsItsContentBetweenTheBounds() {
        #expect(LayoutPolicy.sidebarWidth(forContent: 300) == 300)
    }

    @Test func sidebarWidthClampsToTheBounds() {
        #expect(LayoutPolicy.sidebarWidth(forContent: 0) == LayoutPolicy.sidebarMinimumWidth)
        #expect(LayoutPolicy.sidebarWidth(forContent: 4000) == LayoutPolicy.sidebarMaximumWidth)
    }

    @Test func theSidebarFitsBesideTheDetailPaneAtTheThreshold() {
        #expect(LayoutPolicy.sidebarMaximumWidth < LayoutPolicy.sidebarWindowWidth)
        #expect(LayoutPolicy.sidebarMinimumWidth < LayoutPolicy.sidebarMaximumWidth)
    }

    @Test func theSidebarOpensOnlyWhereBothPaneFloorsAndTheDividerFit() {
        #expect(
            LayoutPolicy.sidebarWindowWidth
                >= LayoutPolicy.sidebarMinimumWidth + LayoutPolicy.contentMinimumWidth + 1)
    }

    @Test func theSmallestWindowHoldsTheDetailPaneAndCanCollapseTheSidebar() {
        #expect(LayoutPolicy.windowMinimumWidth >= LayoutPolicy.contentMinimumWidth)
        #expect(LayoutPolicy.windowMinimumWidth < LayoutPolicy.sidebarWindowWidth)
        let smallest = CGSize(
            width: LayoutPolicy.windowMinimumWidth, height: LayoutPolicy.windowMinimumHeight)
        #expect(!LayoutPolicy.layout(for: smallest).showsSidebar)
    }

    @Test func theChangesFloorsFitTheSmallestWindow() {
        #expect(LayoutPolicy.changesDiffFloor > 0)
        #expect(LayoutPolicy.changesDiffFloor <= LayoutPolicy.windowMinimumHeight)
        let commitColumn =
            LayoutPolicy.changesFilesFloor + LayoutPolicy.changesStashHeight
            + LayoutPolicy.changesMessageHeight
        #expect(LayoutPolicy.changesFilesFloor > 0)
        #expect(commitColumn <= LayoutPolicy.windowMinimumHeight)
    }

    @Test func splitPaneKeepsADividerBothFloorsAllow() {
        #expect(
            LayoutPolicy.splitPane(
                available: 800, preferred: 300, firstFloor: 100, secondFloor: 150) == 300)
    }

    @Test func splitPaneLiftsADividerOffTheFirstFloor() {
        #expect(
            LayoutPolicy.splitPane(
                available: 800, preferred: 10, firstFloor: 100, secondFloor: 150) == 100)
    }

    @Test func splitPaneKeepsTheSecondPaneOffZero() {
        // The poisoned autosave: the divider parked at the bottom, the diff pane at zero.
        #expect(
            LayoutPolicy.splitPane(
                available: 763, preferred: 763, firstFloor: 100, secondFloor: 150) == 613)
    }

    @Test func splitPaneSharesSpaceTooSmallForBothFloors() {
        let first = LayoutPolicy.splitPane(
            available: 50, preferred: 50, firstFloor: 100, secondFloor: 150)
        #expect(first == 20)
        #expect(50 - first == 30)
    }

    @Test func splitPaneHasNoNegativeOrZeroSpaceCase() {
        #expect(
            LayoutPolicy.splitPane(
                available: 0, preferred: 400, firstFloor: 100, secondFloor: 150) == 0)
        #expect(
            LayoutPolicy.splitPane(
                available: -10, preferred: 400, firstFloor: 100, secondFloor: 150) == 0)
    }

    @Test func theHistoryColumnFloorsLeaveBothPanesUsable() {
        let available = LayoutPolicy.historyCommitsFloor + LayoutPolicy.historyDetailFloor
        let commits = LayoutPolicy.historyCommitsHeight(available: available, detailHeight: 0)
        #expect(commits == LayoutPolicy.historyCommitsFloor)
        #expect(available - commits == LayoutPolicy.historyDetailFloor)
    }

    @Test func historyKeepsADetailHeightBothFloorsAllow() {
        #expect(LayoutPolicy.historyCommitsHeight(available: 800, detailHeight: 300) == 500)
        #expect(
            LayoutPolicy.historyCommitsHeight(
                available: 800, detailHeight: LayoutPolicy.historyDetailFloor)
                == 800 - LayoutPolicy.historyDetailFloor)
    }

    @Test func historyOpensAnUnsizedDetailAtItsShare() {
        // Hidden until a commit opened: the detail arrives at zero, or just under its floor.
        let expected = 1000 - 1000 * LayoutPolicy.historyDetailShare
        #expect(LayoutPolicy.historyCommitsHeight(available: 1000, detailHeight: 0) == expected)
        #expect(
            LayoutPolicy.historyCommitsHeight(
                available: 1000, detailHeight: LayoutPolicy.historyDetailFloor - 1) == expected)
    }

    @Test func historyKeepsTheCommitListOffItsFloor() {
        // A detail dragged or restored taller than the column leaves the list its floor.
        #expect(
            LayoutPolicy.historyCommitsHeight(available: 600, detailHeight: 900)
                == LayoutPolicy.historyCommitsFloor)
    }

    @Test func historyKeepsADiffWidthBothFloorsAllow() {
        #expect(LayoutPolicy.historyListWidth(available: 1000, diffWidth: 400) == 600)
        #expect(
            LayoutPolicy.historyListWidth(
                available: 1000, diffWidth: LayoutPolicy.historyDiffMinWidth)
                == 1000 - LayoutPolicy.historyDiffMinWidth)
    }

    @Test func historyOpensAnUnsizedDiffAtItsShare() {
        // Hidden until a commit opened: the diff arrives at zero, or just under its floor.
        let expected = 1000 - 1000 * LayoutPolicy.historyDiffShare
        #expect(LayoutPolicy.historyListWidth(available: 1000, diffWidth: 0) == expected)
        #expect(
            LayoutPolicy.historyListWidth(
                available: 1000, diffWidth: LayoutPolicy.historyDiffMinWidth - 1) == expected)
    }

    @Test func historyKeepsTheListOffItsWidthFloor() {
        // A diff dragged or restored wider than the split leaves the list its floor.
        #expect(
            LayoutPolicy.historyListWidth(available: 900, diffWidth: 900)
                == LayoutPolicy.historyListMinWidth)
    }

    @Test func historyKeepsBothPanesOffZeroInANarrowContentPane() {
        // The narrowest content pane beside the sidebar is far below both width floors.
        let available = LayoutPolicy.contentMinimumWidth - 1
        let list = LayoutPolicy.historyListWidth(available: available, diffWidth: 500)
        #expect(list > 0)
        #expect(available - list > 0)
        let share =
            LayoutPolicy.historyListMinWidth
            / (LayoutPolicy.historyListMinWidth + LayoutPolicy.historyDiffMinWidth)
        #expect(abs(list - available * share) < 0.001)
    }

    private static let wide = Layout(showsSidebar: true)
    private static let narrow = Layout(showsSidebar: false)

    @Test func aPolicyCollapseReopensWhenTheWindowWidens() {
        var collapse = SidebarCollapse()
        #expect(collapse.apply(Self.narrow, previous: Self.wide, isCollapsed: false) == true)
        #expect(collapse.collapsedByPolicy)
        #expect(collapse.apply(Self.wide, previous: Self.narrow, isCollapsed: true) == false)
        #expect(!collapse.collapsedByPolicy)
    }

    @Test func aSidebarTheUserClosedAfterAPolicyCollapseStaysClosed() {
        var collapse = SidebarCollapse()
        #expect(collapse.apply(Self.narrow, previous: Self.wide, isCollapsed: false) == true)
        collapse.userToggled()  // opened by hand while narrow
        collapse.userToggled()  // closed by hand again
        #expect(collapse.apply(Self.wide, previous: Self.narrow, isCollapsed: true) == nil)
    }

    @Test func aManualCollapseWhileWideNeverReopens() {
        var collapse = SidebarCollapse()
        #expect(collapse.apply(Self.wide, previous: nil, isCollapsed: false) == nil)
        collapse.userToggled()
        #expect(collapse.apply(Self.narrow, previous: Self.wide, isCollapsed: true) == nil)
        #expect(collapse.apply(Self.wide, previous: Self.narrow, isCollapsed: true) == nil)
    }

    @Test func onlyADividerDragPinsTheSidebarWidth() {
        let drag: [AnyHashable: Any] = [
            "NSSplitViewDividerIndex": NSNumber(value: 0),
            "NSSplitViewUserResizeKey": NSNumber(value: 1),
        ]
        let setPosition: [AnyHashable: Any] = [
            "NSSplitViewDividerIndex": NSNumber(value: 0),
            "NSSplitViewUserResizeKey": NSNumber(value: 0),
        ]
        let windowResize: [AnyHashable: Any] = ["NSSplitViewDividerIndex": NSNumber(value: 0)]
        #expect(SidebarResize.isUserDrag(drag))
        #expect(!SidebarResize.isUserDrag(setPosition))
        #expect(!SidebarResize.isUserDrag(windowResize))
        #expect(!SidebarResize.isUserDrag(nil))
    }
}

@Suite struct SidebarFitTests {
    @Test func fitAppliesOnlyToAnOpenUnpinnedSidebarShowingFiles() {
        #expect(SidebarFit.applies(isPinned: false, showsFiles: true, isCollapsed: false))
        #expect(!SidebarFit.applies(isPinned: true, showsFiles: true, isCollapsed: false))
        #expect(!SidebarFit.applies(isPinned: false, showsFiles: false, isCollapsed: false))
        #expect(!SidebarFit.applies(isPinned: false, showsFiles: true, isCollapsed: true))
    }

    /// Feeds `fit` one state per step, as (showsFiles, isCollapsed), and returns whether each
    /// step asked the tree to measure again.
    private func measures(
        _ steps: [(showsFiles: Bool, isCollapsed: Bool)], isPinned: Bool = false
    ) -> [Bool] {
        var fit = SidebarFit()
        return steps.map {
            fit.update(isPinned: isPinned, showsFiles: $0.showsFiles, isCollapsed: $0.isCollapsed)
        }
    }

    @Test func returningToFilesMeasuresAgainOnce() {
        let steps = [(true, false), (false, false), (true, false), (true, false)]
        #expect(measures(steps) == [true, false, true, false])
    }

    @Test func reopeningTheSidebarMeasuresAgain() {
        let steps = [(true, false), (true, true), (true, false)]
        #expect(measures(steps) == [true, false, true])
    }

    @Test func reopeningTheSidebarOutsideFilesWaitsForFiles() {
        let steps = [(false, true), (false, false), (true, false)]
        #expect(measures(steps) == [false, false, true])
    }

    @Test func aPinnedSidebarNeverMeasuresAgain() {
        let steps = [(true, false), (false, false), (true, false), (true, true), (true, false)]
        #expect(measures(steps, isPinned: true) == [false, false, false, false, false])
    }
}

@Suite struct SidebarWidthPinTests {
    @Test func unpinnedUntilPinnedThenRemembered() throws {
        try withScratchDefaults { defaults in
            #expect(!SidebarWidthPin(defaults: defaults).isPinned)
            SidebarWidthPin(defaults: defaults).pin()
            #expect(SidebarWidthPin(defaults: defaults).isPinned)
        }
    }

    @Test func theFirstKeyNoLongerPins() throws {
        try withScratchDefaults { defaults in
            defaults.set(true, forKey: "rc-git.sidebar-width-pinned")
            #expect(!SidebarWidthPin(defaults: defaults).isPinned)
        }
    }
}
