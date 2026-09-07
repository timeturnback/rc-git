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
}
