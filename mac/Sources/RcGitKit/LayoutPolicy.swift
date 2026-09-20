import CoreGraphics

/// How the panes are arranged. The sidebar carries the navigation for whichever mode is
/// showing — refs, plus the directory tree or the change list — and the content area beside
/// it is the detail, so only the sidebar changes with window size.
public struct Layout: Equatable, Sendable {
    /// Whether the sidebar has room to stay open.
    public var showsSidebar: Bool

    public init(showsSidebar: Bool) {
        self.showsSidebar = showsSidebar
    }
}

/// Chooses a layout for a window size. Pure, so the thresholds are testable without AppKit.
public enum LayoutPolicy {
    /// Below this window width the sidebar collapses: the sidebar and the detail pane no
    /// longer both keep a usable width.
    public static let sidebarWindowWidth: CGFloat = 560
    /// The narrowest the sidebar goes, however short its rows are. The commit row sets the
    /// floor.
    public static let sidebarMinimumWidth: CGFloat = 260
    /// The widest the sidebar goes, however long its rows are. Past this the detail pane
    /// matters more than a path read in full.
    public static let sidebarMaximumWidth: CGFloat = 460

    public static func layout(for size: CGSize) -> Layout {
        Layout(showsSidebar: size.width >= sidebarWindowWidth)
    }

    /// The sidebar width that shows rows `contentWidth` wide, clamped so the detail pane
    /// keeps the rest of the window.
    public static func sidebarWidth(forContent contentWidth: CGFloat) -> CGFloat {
        min(max(contentWidth, sidebarMinimumWidth), sidebarMaximumWidth)
    }
}
