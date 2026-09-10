import Foundation

/// The repositories open in tabs, in tab order plus which tab was selected, persisted
/// through `UserDefaults` so a relaunch reopens the same tabs.
public struct OpenTabs {
    public static let pathsKey = "rc-git.openTabs"
    public static let selectedKey = "rc-git.openTabs.selected"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var paths: [String] {
        defaults.stringArray(forKey: Self.pathsKey) ?? []
    }

    public var selectedIndex: Int {
        Self.clampedSelection(defaults.integer(forKey: Self.selectedKey), count: paths.count)
    }

    public func save(paths: [String], selectedIndex: Int) {
        defaults.set(paths, forKey: Self.pathsKey)
        defaults.set(
            Self.clampedSelection(selectedIndex, count: paths.count), forKey: Self.selectedKey)
    }

    public func clear() {
        defaults.removeObject(forKey: Self.pathsKey)
        defaults.removeObject(forKey: Self.selectedKey)
    }

    /// Keeps a selection inside the list: a saved index that no longer exists (the
    /// repository failed to reopen, the list shrank) falls back to the first tab.
    public static func clampedSelection(_ index: Int, count: Int) -> Int {
        guard count > 0, index >= 0, index < count else { return 0 }
        return index
    }
}
