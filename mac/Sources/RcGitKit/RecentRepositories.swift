import Foundation

/// The repositories opened most recently, newest first, persisted through `UserDefaults`.
public struct RecentRepositories {
    public static let key = "rc-git.recentRepositories"
    public static let capacity = 10

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public var paths: [String] {
        defaults.stringArray(forKey: Self.key) ?? []
    }

    /// Moves `path` to the front, dropping duplicates and anything past the capacity.
    public func note(_ path: String) {
        defaults.set(Self.inserting(path, into: paths), forKey: Self.key)
    }

    public func clear() {
        defaults.removeObject(forKey: Self.key)
    }

    /// Pure version of `note`, for tests.
    public static func inserting(_ path: String, into existing: [String]) -> [String] {
        var updated = existing.filter { $0 != path }
        updated.insert(path, at: 0)
        return Array(updated.prefix(capacity))
    }

    /// How a recent is written in the Open Recent menu and the picker: the home directory as
    /// `~`. The picker matches against this form, so a query typed as shown finds it.
    public static func displayPath(_ path: String) -> String {
        (path as NSString).abbreviatingWithTildeInPath
    }

    /// The recents `query` matches, best first, for the Open Recent Repository picker: every
    /// path in recency order when the query is blank, else the fuzzy matches against
    /// `displayPath` by score with recency breaking ties. The repository's folder is the
    /// path's last component, so typing its name earns `FuzzyMatch`'s file-name bonus.
    public static func matching(_ paths: [String], query: String) -> [String] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
            .precomposedStringWithCanonicalMapping
        if trimmed.isEmpty { return paths }
        var scored: [(score: Int, position: Int, path: String)] = []
        for (position, path) in paths.enumerated() {
            let key = FuzzyMatch.searchKey(displayPath(path))
            if let score = FuzzyMatch.score(key, query: trimmed) {
                scored.append((score, position, path))
            }
        }
        scored.sort { $0.score != $1.score ? $0.score > $1.score : $0.position < $1.position }
        return scored.map(\.path)
    }

    /// What the picker says in place of an empty list: that there are no recents at all, or
    /// that none matches what was typed.
    public static func emptyNote(hasRecents: Bool) -> String {
        hasRecents ? "No recent repository matches" : "No recent repositories"
    }
}
