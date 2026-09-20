import Foundation

/// How the Changes list lays its paths out.
public enum ChangeGrouping: String, Sendable, CaseIterable {
    /// One row per changed file, labeled with the whole path.
    case flat
    /// Files nested under their folders.
    case folders
}

/// One row of the Changes list: a changed file, or a folder holding changed files.
///
/// Reference identity is what `NSOutlineView` keys expansion and selection on, so the rows
/// are built once per status load and handed to the view as they are.
public final class ChangeNode {
    /// What the row shows: the whole path in `.flat`, the file or folder name in `.folders`.
    public let name: String
    /// Repository-relative path with `/` separators; no trailing slash on a folder.
    public let path: String
    /// The change itself; `nil` on a folder row.
    public let entry: StatusEntry?
    public let children: [ChangeNode]
    /// Every changed path at or below this row, in row order. Precomputed: the tick state of
    /// a folder row reads it on every render.
    public let filePaths: [String]

    public var isFolder: Bool { entry == nil }

    init(name: String, path: String, entry: StatusEntry?, children: [ChangeNode]) {
        self.name = name
        self.path = path
        self.entry = entry
        self.children = children
        self.filePaths = entry.map { [$0.path] } ?? children.flatMap(\.filePaths)
    }
}

/// Builds the Changes list's rows from a status report. Pure: no AppKit, no repository.
public enum ChangeTree {
    /// The rows for `entries` under `grouping`. `entries` is expected in path order, which is
    /// the order `StatusReport` returns.
    public static func nodes(_ entries: [StatusEntry], grouping: ChangeGrouping) -> [ChangeNode] {
        switch grouping {
        case .flat:
            return entries.map {
                ChangeNode(name: $0.path, path: $0.path, entry: $0, children: [])
            }
        case .folders:
            return folders(entries)
        }
    }

    /// A folder's files, and the folders under it, while the tree is being built.
    private final class Level {
        var folders: [String: Level] = [:]
        var folderNames: [String] = []
        var files: [StatusEntry] = []

        func folder(_ name: String) -> Level {
            if let existing = folders[name] { return existing }
            let level = Level()
            folders[name] = level
            folderNames.append(name)
            return level
        }
    }

    private static func folders(_ entries: [StatusEntry]) -> [ChangeNode] {
        let root = Level()
        for entry in entries {
            var components = entry.path.split(separator: "/").map(String.init)
            guard !components.isEmpty else { continue }
            components.removeLast()
            var level = root
            for component in components {
                level = level.folder(component)
            }
            level.files.append(entry)
        }
        return rows(of: root, at: "")
    }

    /// The rows of one folder: its subfolders by name, then its own files in report order.
    private static func rows(of level: Level, at path: String) -> [ChangeNode] {
        var rows = level.folderNames.sorted { $0.localizedStandardCompare($1) == .orderedAscending }
            .compactMap { name -> ChangeNode? in
                guard let child = level.folders[name] else { return nil }
                return folderRow(name: name, path: join(path, name), level: child)
            }
        rows += level.files.map {
            ChangeNode(name: lastComponent(of: $0.path), path: $0.path, entry: $0, children: [])
        }
        return rows
    }

    /// One folder row. A folder with no files and a single subfolder merges into it, so a
    /// path like `docs/solutions/ui-bugs` is one row rather than three nested ones.
    private static func folderRow(name: String, path: String, level: Level) -> ChangeNode {
        let children = rows(of: level, at: path)
        if level.files.isEmpty, children.count == 1, let only = children.first, only.isFolder {
            return ChangeNode(
                name: name + "/" + only.name, path: only.path, entry: nil,
                children: only.children)
        }
        return ChangeNode(name: name, path: path, entry: nil, children: children)
    }

    private static func join(_ path: String, _ name: String) -> String {
        path.isEmpty ? name : path + "/" + name
    }

    private static func lastComponent(of path: String) -> String {
        String(path.split(separator: "/").last ?? Substring(path))
    }
}

/// The Changes list's grouping, remembered between launches.
public struct ChangeGroupingStore {
    public static let key = "rc-git.changesGrouping"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var grouping: ChangeGrouping {
        get { ChangeGrouping(rawValue: defaults.string(forKey: Self.key) ?? "") ?? .flat }
        nonmutating set { defaults.set(newValue.rawValue, forKey: Self.key) }
    }
}
