import Foundation

/// Decides what the app-wide back/forward list records. Only the front tab's moves are the
/// user's: a background tab reports too — a repository attaching at launch, a ref refresh —
/// and a tab showing a restored stop reports the restore itself. Neither is a visit.
///
/// A tab is named by an identity the caller picks, such as its window controller's
/// `ObjectIdentifier`. A tab that has no repository yet still has one, so its first stop
/// counts once it is in front.
public struct Navigator: Sendable {
    public private(set) var list = BackForwardList()
    /// The tab in the main window, or `nil` before any tab came to the front.
    private var front: ObjectIdentifier?
    /// How many stops are being restored.
    private var pendingRestores = 0

    public init() {}

    public func canGo(_ direction: BackForwardList.Direction) -> Bool {
        list.canGo(direction)
    }

    /// Records `stop` as a visit when `tab` is in front and no restore is running. Answers
    /// whether it was recorded, which is when the list may have changed.
    @discardableResult
    public mutating func report(_ stop: NavigationStop, from tab: ObjectIdentifier) -> Bool {
        guard pendingRestores == 0, tab == front else { return false }
        list.visit(stop)
        return true
    }

    /// A switch to another tab is a move to where that tab is, `stop`. The same tab becoming
    /// main again, when the app is reactivated, is not. Answers whether a stop was recorded.
    @discardableResult
    public mutating func tabCameToFront(_ tab: ObjectIdentifier, at stop: NavigationStop?)
        -> Bool
    {
        guard tab != front else { return false }
        front = tab
        guard let stop else { return false }
        return report(stop, from: tab)
    }

    /// Steps the mark one stop and answers the stop to show, or `nil` at either end. The
    /// caller shows it in its tab, bracketed by `beginRestore(in:)` and `endRestore()`.
    public mutating func go(_ direction: BackForwardList.Direction) -> NavigationStop? {
        list.go(direction)
    }

    /// `tab` is coming to the front to show a stop: what it reports until `endRestore()` is
    /// the restore, not a visit.
    public mutating func beginRestore(in tab: ObjectIdentifier) {
        front = tab
        pendingRestores += 1
    }

    public mutating func endRestore() {
        pendingRestores = max(0, pendingRestores - 1)
    }

    /// Forgets a closed tab: its stops, and that it was in front. Pass the repository it
    /// showed, or `nil` for a tab that never showed one.
    public mutating func tabClosed(_ tab: ObjectIdentifier, repositoryPath: String?) {
        if front == tab {
            front = nil
        }
        if let repositoryPath {
            list.removeStops(inRepository: repositoryPath)
        }
    }
}
