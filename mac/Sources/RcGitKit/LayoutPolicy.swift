import CoreGraphics

/// How the panes are arranged. The tree always sits left of the file, whatever the window
/// shape, so only the sidebar changes with size.
public struct Layout: Equatable, Sendable {
    /// Whether the refs sidebar has room to stay open.
    public var showsSidebar: Bool

    public init(showsSidebar: Bool) {
        self.showsSidebar = showsSidebar
    }
}

/// Chooses a layout for a window size. Pure, so the thresholds are testable without AppKit.
public enum LayoutPolicy {
    /// Below this width the sidebar collapses so the tree and file keep usable widths.
    public static let sidebarMinimumWidth: CGFloat = 760
    /// The narrowest the tree pane goes, however short its rows are.
    public static let treeMinimumWidth: CGFloat = 160
    /// The widest the tree pane goes, however long its rows are. Past this the file pane
    /// matters more than a path read in full.
    public static let treeMaximumWidth: CGFloat = 460

    public static func layout(for size: CGSize) -> Layout {
        Layout(showsSidebar: size.width >= sidebarMinimumWidth)
    }

    /// The tree pane width that shows rows `contentWidth` wide, clamped so the file pane
    /// keeps the rest of the window.
    public static func treeWidth(forContent contentWidth: CGFloat) -> CGFloat {
        min(max(contentWidth, treeMinimumWidth), treeMaximumWidth)
    }
}
