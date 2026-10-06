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

    /// Replaces the list. Only the restore at launch writes this way, to drop a repository
    /// that no longer opens; everything else records through `record`.
    public func save(paths: [String], selectedIndex: Int) {
        defaults.set(paths, forKey: Self.pathsKey)
        defaults.set(
            Self.clampedSelection(selectedIndex, count: paths.count), forKey: Self.selectedKey)
    }

    /// Records the tabs that are open now without dropping the ones that are not, except
    /// `dropping`: the repository of a tab closed on its own with ⌘W. ⇧⌘W on a whole window
    /// and ⌘Q drop nothing, so a relaunch reopens every repository they closed.
    public func record(live: [String], selectedIndex: Int, dropping: String? = nil) {
        let saved = paths.filter { $0 != dropping }
        save(paths: Self.merged(live: live, saved: saved), selectedIndex: selectedIndex)
    }

    /// Records for a build that is about to replace this one, and forces the write out.
    /// `UserDefaults` hands changes to `cfprefsd` asynchronously, and the relaunched app
    /// reads these keys within milliseconds — while this process is still alive, so the
    /// on-disk domain it reads is whatever was last flushed. Without the flush the new app
    /// reads an older list and opens an empty window.
    public func handOff(live: [String], selectedIndex: Int) {
        record(live: live, selectedIndex: selectedIndex)
        defaults.synchronize()
    }

    public func clear() {
        defaults.removeObject(forKey: Self.pathsKey)
        defaults.removeObject(forKey: Self.selectedKey)
    }

    /// The tabs on screen followed by the saved ones that are not open any more, each path
    /// once. Live order comes first, which is what records a drag reorder; a closed tab keeps
    /// its place after them, so the selected index still counts from the live tabs.
    public static func merged(live: [String], saved: [String]) -> [String] {
        var result: [String] = []
        for path in live + saved where !result.contains(path) { result.append(path) }
        return result
    }

    /// Keeps a selection inside the list: a saved index that no longer exists (the
    /// repository failed to reopen, the list shrank) falls back to the first tab.
    public static func clampedSelection(_ index: Int, count: Int) -> Int {
        guard count > 0, index >= 0, index < count else { return 0 }
        return index
    }
}
