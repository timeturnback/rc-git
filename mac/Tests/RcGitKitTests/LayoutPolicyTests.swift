import CoreGraphics
import Testing

@testable import RcGitKit

@Suite struct LayoutPolicyTests {
    @Test func wideWindowShowsEverythingSideBySide() {
        let layout = LayoutPolicy.layout(for: CGSize(width: 1400, height: 900))
        #expect(layout == Layout(showsSidebar: true, contentAxis: .horizontal))
    }

    @Test func narrowWindowHidesSidebarFirst() {
        let layout = LayoutPolicy.layout(for: CGSize(width: 700, height: 600))
        #expect(layout == Layout(showsSidebar: false, contentAxis: .horizontal))
    }

    @Test func veryNarrowWindowStacksPanes() {
        let layout = LayoutPolicy.layout(for: CGSize(width: 500, height: 500))
        #expect(layout == Layout(showsSidebar: false, contentAxis: .vertical))
    }

    @Test func portraitWindowStacksEvenWhenWide() {
        let layout = LayoutPolicy.layout(for: CGSize(width: 900, height: 1400))
        #expect(layout == Layout(showsSidebar: true, contentAxis: .vertical))
    }

    @Test func thresholdsAreInclusiveAtTheBoundary() {
        let atSidebar = LayoutPolicy.layout(
            for: CGSize(width: LayoutPolicy.sidebarMinimumWidth, height: 400))
        #expect(atSidebar.showsSidebar)
        let belowSidebar = LayoutPolicy.layout(
            for: CGSize(width: LayoutPolicy.sidebarMinimumWidth - 1, height: 400))
        #expect(!belowSidebar.showsSidebar)
        let square = LayoutPolicy.layout(for: CGSize(width: 800, height: 800))
        #expect(square.contentAxis == .horizontal)
    }
}
