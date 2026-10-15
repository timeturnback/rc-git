import CoreGraphics
import Foundation

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
    /// The narrowest the detail pane beside the sidebar goes. The sidebar threshold leaves room
    /// for both floors and the divider between them.
    public static let contentMinimumWidth: CGFloat = 240
    /// The smallest window: the detail pane alone, with the sidebar collapsed, still usable.
    public static let windowMinimumWidth: CGFloat = 440
    public static let windowMinimumHeight: CGFloat = 320
    /// The height neither pane of History's left column may drop below: the commit list on
    /// top, the open commit's detail — header and changed files — under it. A pane at zero
    /// reads as a missing feature, and `NSSplitView` autosave records the zero and re-imposes
    /// it on the next launch.
    public static let historyCommitsFloor: CGFloat = 120
    public static let historyDetailFloor: CGFloat = 200
    /// The share of the left column a commit detail opens at when it has no height of its own.
    public static let historyDetailShare: CGFloat = 0.45
    /// The width neither pane of History's outer split may drop below while a commit is open:
    /// the left column (commit list over the detail) and the diff of the picked file beside it.
    public static let historyListMinWidth: CGFloat = 320
    public static let historyDiffMinWidth: CGFloat = 340
    /// The share of History's width the diff pane opens at when it has no width of its own.
    public static let historyDiffShare: CGFloat = 0.5
    /// The height the Changes tab's file list keeps while the sidebar has room for it. It
    /// yields before the commit box does, since it shares the sidebar with the ref list.
    public static let changesFilesFloor: CGFloat = 150
    /// The fixed heights of the Changes tab's stash list and commit message box.
    public static let changesStashHeight: CGFloat = 96
    public static let changesMessageHeight: CGFloat = 64
    /// The height the Changes tab's diff never drops below.
    public static let changesDiffFloor: CGFloat = 150

    public static func layout(for size: CGSize) -> Layout {
        Layout(showsSidebar: size.width >= sidebarWindowWidth)
    }

    /// The sidebar width that shows rows `contentWidth` wide, clamped so the detail pane
    /// keeps the rest of the window.
    public static func sidebarWidth(forContent contentWidth: CGFloat) -> CGFloat {
        min(max(contentWidth, sidebarMinimumWidth), sidebarMaximumWidth)
    }

    /// The first pane's size along a split's axis, out of the `available` space the two panes
    /// share — the divider already taken off.
    ///
    /// `preferred` is where the divider sits now, kept whenever both floors still fit. Space
    /// too small for both floors is split between them in proportion, so neither pane reaches
    /// zero and the divider stays where a drag can reach it.
    public static func splitPane(
        available: CGFloat, preferred: CGFloat, firstFloor: CGFloat, secondFloor: CGFloat
    ) -> CGFloat {
        guard available > 0 else { return 0 }
        guard available >= firstFloor + secondFloor else {
            return available * firstFloor / (firstFloor + secondFloor)
        }
        return min(max(preferred, firstFloor), available - secondFloor)
    }

    /// The commit list's height in History's left column, out of the `available` height it
    /// shares with the commit detail under it.
    ///
    /// `detailHeight` is the detail's height now, kept whenever both floors allow it. A detail
    /// below its floor has never been sized — it was hidden until a commit opened — so it
    /// opens at `historyDetailShare` of the column instead of squeezed onto the floor.
    public static func historyCommitsHeight(available: CGFloat, detailHeight: CGFloat)
        -> CGFloat
    {
        let detail =
            detailHeight >= historyDetailFloor ? detailHeight : available * historyDetailShare
        return splitPane(
            available: available, preferred: available - detail,
            firstFloor: historyCommitsFloor, secondFloor: historyDetailFloor)
    }

    /// The left column's width in History's outer split, out of the `available` width it
    /// shares with the diff pane beside it.
    ///
    /// `diffWidth` is the diff pane's width now, kept whenever both floors allow it, so a
    /// window resize moves the column and leaves the diff alone. A diff below its floor has
    /// never been sized — it was hidden until a commit opened — so it opens at
    /// `historyDiffShare` of the width instead of squeezed onto the floor.
    public static func historyListWidth(available: CGFloat, diffWidth: CGFloat) -> CGFloat {
        let diff = diffWidth >= historyDiffMinWidth ? diffWidth : available * historyDiffShare
        return splitPane(
            available: available, preferred: available - diff,
            firstFloor: historyListMinWidth, secondFloor: historyDiffMinWidth)
    }
}

/// Whether the window controller collapsed the sidebar itself, so that widening the window
/// re-opens only a sidebar the policy closed and never one the user closed.
public struct SidebarCollapse: Equatable, Sendable {
    public private(set) var collapsedByPolicy = false

    public init() {}

    /// The collapsed state the sidebar should take for `layout`, or `nil` to leave it as it is.
    ///
    /// A window too narrow for the sidebar collapses an open one and remembers doing so; a
    /// window widening past the threshold re-opens it only when that collapse is still the
    /// policy's.
    public mutating func apply(_ layout: Layout, previous: Layout?, isCollapsed: Bool) -> Bool? {
        if !layout.showsSidebar, !isCollapsed {
            collapsedByPolicy = true
            return true
        }
        if layout.showsSidebar, previous?.showsSidebar == false, collapsedByPolicy {
            collapsedByPolicy = false
            return false
        }
        return nil
    }

    /// The user opened or closed the sidebar by hand: whatever state it is in now is theirs.
    public mutating func userToggled() {
        collapsedByPolicy = false
    }
}

/// Whether the sidebar sizes itself to the tree's rows, and when that turns back on. The tree
/// measures nothing while the fit stands back, so a width skipped then must be measured again
/// the moment Files shows or the sidebar re-opens, or the sidebar keeps its stale width.
public struct SidebarFit: Equatable, Sendable {
    public private(set) var applies = false

    public init() {}

    /// Whether the fit applies: a divider the user dragged is theirs, only Files shows the tree,
    /// and a width for a collapsed sidebar would re-open it behind the user.
    public static func applies(isPinned: Bool, showsFiles: Bool, isCollapsed: Bool) -> Bool {
        !isPinned && showsFiles && !isCollapsed
    }

    /// Records the fit's state now; `true` when it has just turned on and the tree must
    /// measure its rows again.
    public mutating func update(isPinned: Bool, showsFiles: Bool, isCollapsed: Bool) -> Bool {
        let now = Self.applies(isPinned: isPinned, showsFiles: showsFiles, isCollapsed: isCollapsed)
        defer { applies = now }
        return now && !applies
    }
}

/// Reads `NSSplitView.didResizeSubviewsNotification` user info.
public enum SidebarResize {
    /// Whether the resize was the user dragging a divider. `setPosition(_:ofDividerAt:)` posts
    /// the user-resize key as 0 and a window resize posts no key, so only a drag counts —
    /// the automatic fit and a frame restore must not pin the width.
    public static func isUserDrag(_ userInfo: [AnyHashable: Any]?) -> Bool {
        (userInfo?["NSSplitViewUserResizeKey"] as? Int) == 1
    }
}

/// Whether the user has dragged the sidebar divider, remembered between launches. Once pinned,
/// the automatic fit stands back for good.
public struct SidebarWidthPin {
    /// .v2: the first key was also set by the fit's own `setPosition`, so it pins nothing.
    public static let key = "rc-git.sidebar-width-pinned.v2"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var isPinned: Bool { defaults.bool(forKey: Self.key) }

    public func pin() {
        defaults.set(true, forKey: Self.key)
    }
}
