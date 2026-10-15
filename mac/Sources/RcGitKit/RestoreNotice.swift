import Foundation

/// What the banner says after a launch restore left saved tabs closed: one line naming each
/// repository, and a tooltip with every full path and what happens to it next launch.
public struct RestoreNotice: Sendable, Hashable {
    public let summary: String
    public let detail: String

    /// The notice for a restore where `failed` did not open and `kept` is the saved list it
    /// wrote (see `OpenTabs.restoredPaths`). A failed path still in `kept` is on a volume that
    /// is not mounted and is retried next launch; any other failed path was dropped. Nil when
    /// every saved tab opened.
    public static func make(failed: [String], kept: [String]) -> RestoreNotice? {
        guard !failed.isEmpty else { return nil }
        let count = failed.count == 1 ? "1 saved tab" : "\(failed.count) saved tabs"
        let names = failed.map { path in
            "\(name(of: path)) (\(fate(of: path, kept: kept)))"
        }
        let lines = failed.map { path in
            "\(path): \(fate(of: path, kept: kept))"
        }
        return RestoreNotice(
            summary: "Could not reopen \(count): " + names.joined(separator: ", "),
            detail: lines.joined(separator: "\n"))
    }

    private static func fate(of path: String, kept: [String]) -> String {
        kept.contains(path) ? "not reachable, will retry next launch" : "removed from saved tabs"
    }

    private static func name(of path: String) -> String {
        let name = URL(filePath: path).lastPathComponent
        return name.isEmpty || name == "/" ? path : name
    }
}
