import Foundation
import os

/// The repositories open in tabs, in tab order plus which tab was selected, persisted
/// through `UserDefaults` so a relaunch reopens the same tabs.
public struct OpenTabs {
    public static let pathsKey = "rc-git.openTabs"
    public static let selectedKey = "rc-git.openTabs.selected"

    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "rcgit", category: "tabs")

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
    /// that no longer opens (see `restoredPaths`); everything else records through `record`.
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
        let count = live.count
        if defaults.synchronize() {
            Self.logger.notice("hand-off: flushed \(count) open tabs")
        } else {
            Self.logger.error(
                "hand-off: could not flush \(count) open tabs; the new build may read older tabs")
        }
    }

    /// The tabs on screen followed by the saved ones that are not open any more, each path
    /// once. Live order comes first, which is what records a drag reorder; a closed tab keeps
    /// its place after them, so the selected index still counts from the live tabs.
    public static func merged(live: [String], saved: [String]) -> [String] {
        var result: [String] = []
        for path in live + saved where !result.contains(path) { result.append(path) }
        return result
    }

    /// The list to save after a launch restore: the tabs that opened, then each saved path
    /// that failed to open while it was unreachable (see `mountFolder`) — an external disk or
    /// a network share not mounted yet. Such a path is kept for the next launch. A failed path
    /// whose mount folder exists is dropped: the volume is there and the repository is not,
    /// so retrying it on every launch would fail the same way.
    public static func restoredPaths(
        opened: [String], failed: [String],
        directoryExists: (String) -> Bool = directoryExists
    ) -> [String] {
        merged(live: opened, saved: failed.filter { !directoryExists(mountFolder(of: $0)) })
    }

    /// The folder whose absence means `path` is unreachable rather than gone: the path itself
    /// when it is a volume root (`/Volumes/Ext`), since `/Volumes` stays put while the disk is
    /// unmounted; otherwise the folder that contains it.
    public static func mountFolder(of path: String) -> String {
        let url = URL(filePath: path)
        let parent = url.deletingLastPathComponent()
        if parent.standardizedFileURL.path(percentEncoded: false) == volumesFolder {
            return url.path(percentEncoded: false)
        }
        return parent.path(percentEncoded: false)
    }

    private static let volumesFolder = "/Volumes/"

    /// Whether a folder exists at `path`.
    public static func directoryExists(_ path: String) -> Bool {
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory)
            && isDirectory.boolValue
    }

    /// The selection after a restore that skipped repositories which no longer open: the
    /// index in `opened` of the path that was selected in `saved`, or the first tab when that
    /// path is the one that failed to open.
    ///
    /// `launchDocument` is the repository whose opening launched the app (Finder, the Dock,
    /// `open -a`). It is what the user asked for, so it stays selected over the saved choice.
    public static func restoredSelection(
        saved: [String], selectedIndex: Int, opened: [String], launchDocument: String? = nil
    ) -> Int {
        if let launchDocument, let index = opened.firstIndex(of: launchDocument) {
            return index
        }
        guard !saved.isEmpty else { return 0 }
        let path = saved[clampedSelection(selectedIndex, count: saved.count)]
        return opened.firstIndex(of: path) ?? 0
    }

    /// Keeps a selection inside the list: a saved index that no longer exists (the
    /// repository failed to reopen, the list shrank) falls back to the first tab.
    public static func clampedSelection(_ index: Int, count: Int) -> Int {
        guard count > 0, index >= 0, index < count else { return 0 }
        return index
    }
}
