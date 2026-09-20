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

    /// Saves for a build that is about to replace this one, and forces the write out.
    /// `UserDefaults` hands changes to `cfprefsd` asynchronously, and the relaunched app
    /// reads these keys within milliseconds — while this process is still alive, so the
    /// on-disk domain it reads is whatever was last flushed. Without the flush the new app
    /// reads an older list and opens an empty window.
    public func handOff(paths: [String], selectedIndex: Int) {
        save(paths: paths, selectedIndex: selectedIndex)
        defaults.synchronize()
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
