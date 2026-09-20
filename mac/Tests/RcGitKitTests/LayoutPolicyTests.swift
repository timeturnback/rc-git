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
}
