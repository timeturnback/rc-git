import Foundation

/// Fingerprints of ssh host keys the user chose to trust, one per host. rc-git never
/// writes `~/.ssh/known_hosts`; approvals live app-side. The production implementation
/// is `UserDefaults` — fingerprints are public values, not secrets.
public protocol HostKeyStoring: Sendable {
    func fingerprint(host: String) -> String?
    func store(fingerprint: String, host: String)
    /// Drops the approval for `host`, so a legitimately rotated key can be approved anew.
    func forget(host: String)
}

/// `UserDefaults`-backed store: one dictionary of host → `SHA256:...` fingerprint.
///
/// `@unchecked Sendable` because `UserDefaults` is documented thread-safe and the struct holds
/// nothing else. Entries are read one by one, so a value that is not a string under one host
/// neither hides nor wipes the approvals stored for the others.
public struct UserDefaultsHostKeyStore: HostKeyStoring, @unchecked Sendable {
    private static let key = "rcgit.acceptedHostKeys"
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func fingerprint(host: String) -> String? {
        approvals()[host]
    }

    public func store(fingerprint: String, host: String) {
        var stored = approvals()
        stored[host] = fingerprint
        defaults.set(stored, forKey: Self.key)
    }

    public func forget(host: String) {
        var stored = approvals()
        stored[host] = nil
        defaults.set(stored, forKey: Self.key)
    }

    private func approvals() -> [String: String] {
        (defaults.dictionary(forKey: Self.key) ?? [:]).compactMapValues { $0 as? String }
    }
}
