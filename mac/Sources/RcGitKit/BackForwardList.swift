import Foundation

/// Which pane fills the content area beside the sidebar.
public enum ContentMode: Int, Sendable {
    case files = 0
    case history = 1
    case changes = 2
}

/// What a tab's tree pane browses.
public enum BrowsedTree: Equatable, Sendable {
    /// No repository, or one with no ref to select.
    case nothing
    case workingTree
    /// A ref by its full name, so returning to it follows the ref when it has moved.
    case ref(String)
    /// One commit picked from the history, and the tip whose history the pane kept showing.
    case commit(Oid, historyTip: Oid?)
}

/// Where one tab's browser is: the tree it browses, the file in the file pane, what the
/// history pane lists, and the commit in its detail. It names things rather than holding
/// them, so going back to one reads the repository as it is then.
public struct BrowserLocation: Equatable, Sendable {
    public var tree: BrowsedTree
    public var filePath: String?
    public var historyScope: HistoryScope
    public var detailCommit: Oid?

    public init(
        tree: BrowsedTree = .nothing, filePath: String? = nil, historyScope: HistoryScope = .all,
        detailCommit: Oid? = nil
    ) {
        self.tree = tree
        self.filePath = filePath
        self.historyScope = historyScope
        self.detailCommit = detailCommit
    }
}

/// One stop of the back/forward list: a tab, the pane it shows, and where its browser is.
public struct NavigationStop: Equatable, Sendable {
    /// The repository of the tab, which is what names a tab: a repository opens in one only.
    public var repositoryPath: String
    public var mode: ContentMode
    public var browser: BrowserLocation

    public init(repositoryPath: String, mode: ContentMode, browser: BrowserLocation) {
        self.repositoryPath = repositoryPath
        self.mode = mode
        self.browser = browser
    }
}

/// The stops the user went through, across every tab, with the one on screen marked. It
/// works the way a browser's does: a move somewhere new drops the stops ahead, and stepping
/// back or forward only moves the mark.
public struct BackForwardList: Equatable, Sendable {
    public enum Direction: Sendable {
        case back
        case forward

        /// The fourth and fifth buttons of a mouse, which AppKit numbers 3 and 4.
        public init?(mouseButton: Int) {
            switch mouseButton {
            case 3: self = .back
            case 4: self = .forward
            default: return nil
            }
        }

        /// A finished swipe, by the amount the gesture tracking reports: positive uncovers the
        /// stop before this one. `nil` for a swipe that went nowhere.
        public init?(swipeAmount: Double) {
            if swipeAmount > 0 {
                self = .back
            } else if swipeAmount < 0 {
                self = .forward
            } else {
                return nil
            }
        }
    }

    /// How many stops the list keeps; the oldest fall off past it.
    public static let limit = 200

    public private(set) var stops: [NavigationStop] = []
    /// Which of `stops` is on screen. Inside `stops` whenever there are any.
    public private(set) var index = 0

    public init() {}

    public var current: NavigationStop? {
        stops.indices.contains(index) ? stops[index] : nil
    }

    public func canGo(_ direction: Direction) -> Bool {
        switch direction {
        case .back: index > 0
        case .forward: index < stops.count - 1
        }
    }

    /// Records a move to `stop`. A move to where the list already is records nothing;
    /// anything else drops the stops ahead of the current one.
    public mutating func visit(_ stop: NavigationStop) {
        guard stop != current else { return }
        if !stops.isEmpty {
            stops.removeSubrange((index + 1)...)
        }
        stops.append(stop)
        if stops.count > Self.limit {
            stops.removeFirst(stops.count - Self.limit)
        }
        index = stops.count - 1
    }

    /// Steps the mark one stop and answers the stop to show, or `nil` at either end.
    public mutating func go(_ direction: Direction) -> NavigationStop? {
        guard canGo(direction) else { return nil }
        index += direction == .back ? -1 : 1
        return stops[index]
    }

    /// Drops every stop in one repository, which is what closing its tab does. The mark stays
    /// on the stop it was on, or falls back to the nearest one before it; two stops the
    /// dropped ones kept apart become one when they are the same place.
    public mutating func removeStops(inRepository path: String) {
        var kept: [NavigationStop] = []
        var keptIndex = 0
        for (offset, stop) in stops.enumerated() where stop.repositoryPath != path {
            if kept.last != stop {
                kept.append(stop)
            }
            if offset <= index {
                keptIndex = kept.count - 1
            }
        }
        stops = kept
        index = keptIndex
    }

    /// Whether stepping back shows another commit in the same tab's history pane, which is
    /// when the commit detail offers its own back arrow.
    public func backReturnsToCommit(inRepository path: String) -> Bool {
        guard canGo(.back), let current else { return false }
        let previous = stops[index - 1]
        return current.repositoryPath == path && previous.repositoryPath == path
            && current.mode == .history && previous.mode == .history
            && previous.browser.detailCommit != nil
            && previous.browser.detailCommit != current.browser.detailCommit
    }
}
