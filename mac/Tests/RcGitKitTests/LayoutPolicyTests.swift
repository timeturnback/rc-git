import CoreGraphics
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

    @Test func theHistoryDrawerFloorsLeaveBothPanesUsable() {
        let available = LayoutPolicy.historyChangesFloor + LayoutPolicy.historyDiffFloor
        let changes = LayoutPolicy.splitPane(
            available: available, preferred: 0, firstFloor: LayoutPolicy.historyChangesFloor,
            secondFloor: LayoutPolicy.historyDiffFloor)
        #expect(changes == LayoutPolicy.historyChangesFloor)
        #expect(available - changes == LayoutPolicy.historyDiffFloor)
    }
}
