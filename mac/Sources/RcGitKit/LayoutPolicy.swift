import CoreGraphics

/// How the three panes are arranged.
public struct Layout: Equatable, Sendable {
    /// Whether the refs sidebar has room to stay open.
    public var showsSidebar: Bool
    /// Whether the tree and the file sit side by side or the tree sits above the file.
    public var contentAxis: ContentAxis

    public init(showsSidebar: Bool, contentAxis: ContentAxis) {
        self.showsSidebar = showsSidebar
        self.contentAxis = contentAxis
    }
}

public enum ContentAxis: Equatable, Sendable {
    case horizontal
    case vertical
}

/// Chooses a layout for a window size. Pure, so the thresholds are testable without AppKit.
public enum LayoutPolicy {
    /// Below this width the sidebar collapses so the tree and file keep usable widths.
    public static let sidebarMinimumWidth: CGFloat = 760
    /// Below this width the tree stacks above the file regardless of aspect ratio.
    public static let stackBelowWidth: CGFloat = 560
    /// A window taller than this many times its width stacks the panes.
    public static let portraitRatio: CGFloat = 1.15

    public static func layout(for size: CGSize) -> Layout {
        let stacked = size.width < stackBelowWidth || size.height > size.width * portraitRatio
        return Layout(
            showsSidebar: size.width >= sidebarMinimumWidth,
            contentAxis: stacked ? .vertical : .horizontal
        )
    }
}
